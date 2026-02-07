"""Static thread pools for the parallel package pipeline.

Provides three resource-isolated thread pools:
- DownloadPool: Network I/O (HTTP downloads with progress tracking)
- UnpackPool: Disk I/O (archive extraction with progress tracking)
- InstallPool: CPU (verification, fingerprinting, post-install hooks)

Each pool wraps a ThreadPoolExecutor and integrates with the existing
PackageDownloader/ArchiveExtractor infrastructure while reporting progress
through the ProgressCallback protocol.
"""

import hashlib
import logging
import platform
import threading
import time
from concurrent.futures import Future, ThreadPoolExecutor
from pathlib import Path
from typing import Any

from .callbacks import ProgressCallback
from .models import PackageTask, TaskPhase

logger = logging.getLogger(__name__)

# Download retry configuration
_MAX_DOWNLOAD_RETRIES = 3
_RETRY_BACKOFF_BASE = 1.0  # seconds; delays are 1s, 2s, 4s

# Extraction retry configuration (Windows AV delays)
_MAX_EXTRACT_RETRIES = 3
_EXTRACT_RETRY_DELAY = 2.0  # seconds between retries


class DownloadPool:
    """Thread pool for downloading packages over the network.

    Wraps ThreadPoolExecutor with download-specific logic: streaming HTTP
    downloads with chunk-based progress reporting through a ProgressCallback.

    Args:
        max_workers: Maximum concurrent downloads.
    """

    def __init__(self, max_workers: int) -> None:
        self._max_workers = max_workers
        self._executor = ThreadPoolExecutor(max_workers=max_workers, thread_name_prefix="download")
        self._shutdown = False
        self._lock = threading.Lock()

    def submit_download(self, task: PackageTask, callback: ProgressCallback) -> Future[Path]:
        """Submit a download job for the given package task.

        Downloads the file from task.url to task.dest_path parent directory,
        reporting chunk-level progress through the callback.

        Args:
            task: Package task containing url and dest_path.
            callback: Progress callback for reporting download progress.

        Returns:
            Future resolving to the path of the downloaded archive file.

        Raises:
            RuntimeError: If the pool has been shut down.
        """
        with self._lock:
            if self._shutdown:
                raise RuntimeError("DownloadPool has been shut down")
        return self._executor.submit(self._do_download, task, callback)

    def _do_download(self, task: PackageTask, callback: ProgressCallback) -> Path:
        """Execute the actual download in a worker thread with retry logic.

        Uses the requests library for streaming HTTP downloads with
        chunk-based SHA256 hashing and progress callback invocation.
        Retries on transient network failures with exponential backoff.

        Args:
            task: Package task to download.
            callback: Progress callback.

        Returns:
            Path to the downloaded archive file.

        Raises:
            ImportError: If requests library is not available.
            Exception: On download failure after all retries exhausted.
        """
        try:
            import requests
        except ImportError:
            raise ImportError("requests is required for downloading. Install with: pip install requests")

        dest_path = Path(task.dest_path)
        archive_name = Path(task.url.split("/")[-1].split("?")[0]).name
        archive_path = dest_path.parent / archive_name

        # Ensure parent directory exists
        archive_path.parent.mkdir(parents=True, exist_ok=True)

        # Use .download temp extension to avoid antivirus interference
        temp_file = Path(str(archive_path) + ".download")

        last_error: Exception | None = None

        for attempt in range(_MAX_DOWNLOAD_RETRIES):
            try:
                if attempt > 0:
                    delay = _RETRY_BACKOFF_BASE * (2 ** (attempt - 1))
                    callback.on_progress(
                        task.name,
                        TaskPhase.DOWNLOADING,
                        0,
                        0,
                        f"Retry {attempt}/{_MAX_DOWNLOAD_RETRIES - 1} after {delay:.0f}s...",
                    )
                    time.sleep(delay)

                archive_path = self._do_download_attempt(task, callback, archive_path, temp_file)
                return archive_path

            except requests.HTTPError:
                # HTTP errors (404, 500, etc.) are not transient - don't retry
                _cleanup_temp_file(temp_file)
                raise
            except (requests.ConnectionError, requests.Timeout, OSError) as e:
                last_error = e
                logger.warning("Download attempt %d/%d failed for %s: %s", attempt + 1, _MAX_DOWNLOAD_RETRIES, task.name, e)

                # Cleanup partial temp file
                _cleanup_temp_file(temp_file)

                if attempt == _MAX_DOWNLOAD_RETRIES - 1:
                    raise
            except KeyboardInterrupt:
                _cleanup_temp_file(temp_file)
                raise
            except Exception:
                # Other unexpected errors - don't retry
                _cleanup_temp_file(temp_file)
                raise

        # Should not reach here, but satisfy type checker
        raise last_error  # type: ignore[misc]

    def _do_download_attempt(
        self,
        task: PackageTask,
        callback: ProgressCallback,
        archive_path: Path,
        temp_file: Path,
    ) -> Path:
        """Execute a single download attempt.

        Args:
            task: Package task to download.
            callback: Progress callback.
            archive_path: Final archive file path.
            temp_file: Temporary download file path.

        Returns:
            Path to the downloaded archive file.
        """
        import requests

        response = requests.get(task.url, stream=True, timeout=30)
        response.raise_for_status()

        total_size = int(response.headers.get("content-length", 0))
        downloaded = 0
        sha256 = hashlib.sha256()

        callback.on_progress(task.name, TaskPhase.DOWNLOADING, 0, total_size, "Starting download...")

        with open(temp_file, "wb") as f:
            for chunk in response.iter_content(chunk_size=8192):
                if chunk:
                    f.write(chunk)
                    sha256.update(chunk)
                    downloaded += len(chunk)
                    detail = _format_transfer_speed(downloaded, task)
                    callback.on_progress(task.name, TaskPhase.DOWNLOADING, downloaded, total_size, detail)

        # Post-download stabilization (Windows)
        if platform.system() == "Windows":
            import gc

            gc.collect()
            time.sleep(0.2)

        # Move temp to final path
        if archive_path.exists():
            try:
                archive_path.unlink()
            except (PermissionError, OSError):
                pass  # Will be handled by rename

        try:
            temp_file.rename(archive_path)
        except (PermissionError, OSError):
            import shutil

            shutil.copy2(str(temp_file), str(archive_path))
            try:
                temp_file.unlink()
            except (PermissionError, OSError):
                pass

        callback.on_progress(task.name, TaskPhase.DOWNLOADING, total_size, total_size, "Download complete")
        return archive_path

    def shutdown(self) -> None:
        """Shut down the thread pool, waiting for all pending downloads to finish."""
        with self._lock:
            self._shutdown = True
        self._executor.shutdown(wait=True)

    @property
    def max_workers(self) -> int:
        """Maximum number of concurrent download workers."""
        return self._max_workers

    def __enter__(self) -> "DownloadPool":
        return self

    def __exit__(self, exc_type: Any, exc_val: Any, exc_tb: Any) -> None:
        self.shutdown()


class UnpackPool:
    """Thread pool for extracting package archives.

    Wraps ThreadPoolExecutor with extraction-specific logic: archive
    extraction with file-count progress reporting.

    Args:
        max_workers: Maximum concurrent extraction operations.
    """

    def __init__(self, max_workers: int) -> None:
        self._max_workers = max_workers
        self._executor = ThreadPoolExecutor(max_workers=max_workers, thread_name_prefix="unpack")
        self._shutdown = False
        self._lock = threading.Lock()

    def submit_unpack(self, task: PackageTask, archive_path: Path, callback: ProgressCallback) -> Future[Path]:
        """Submit an unpack job for the given archive.

        Extracts the archive to a directory derived from task.dest_path,
        reporting file-level progress through the callback.

        Args:
            task: Package task being unpacked.
            archive_path: Path to the downloaded archive file.
            callback: Progress callback for reporting extraction progress.

        Returns:
            Future resolving to the path of the extracted directory.

        Raises:
            RuntimeError: If the pool has been shut down.
        """
        with self._lock:
            if self._shutdown:
                raise RuntimeError("UnpackPool has been shut down")
        return self._executor.submit(self._do_unpack, task, archive_path, callback)

    def _do_unpack(self, task: PackageTask, archive_path: Path, callback: ProgressCallback) -> Path:
        """Execute the actual archive extraction in a worker thread.

        Supports .tar.xz, .tar.gz, and .zip archives. Reports progress based
        on the number of members/entries extracted. Retries on PermissionError
        to handle Windows antivirus delays.

        Args:
            task: Package task being unpacked.
            archive_path: Path to the archive file.
            callback: Progress callback.

        Returns:
            Path to the extracted directory.

        Raises:
            ValueError: If archive format is not supported.
            Exception: On extraction failure after all retries exhausted.
        """
        import gc
        import shutil

        dest_path = Path(task.dest_path)
        dest_path.mkdir(parents=True, exist_ok=True)

        last_error: Exception | None = None

        for attempt in range(_MAX_EXTRACT_RETRIES):
            # Create temp extraction directory
            temp_extract = dest_path.parent / f"temp_extract_{archive_path.name}"
            if temp_extract.exists():
                shutil.rmtree(temp_extract, ignore_errors=True)
            temp_extract.mkdir(parents=True, exist_ok=True)

            try:
                if attempt > 0:
                    callback.on_progress(
                        task.name,
                        TaskPhase.UNPACKING,
                        0,
                        0,
                        f"Retry {attempt}/{_MAX_EXTRACT_RETRIES - 1} after extraction error...",
                    )
                    time.sleep(_EXTRACT_RETRY_DELAY)
                else:
                    callback.on_progress(task.name, TaskPhase.UNPACKING, 0, 0, "Starting extraction...")

                archive_str = str(archive_path).lower()

                if archive_str.endswith(".tar.xz") or archive_str.endswith(".txz"):
                    self._extract_tar(archive_path, temp_extract, "r:xz", task, callback)
                elif archive_str.endswith(".tar.gz") or archive_str.endswith(".tgz"):
                    self._extract_tar(archive_path, temp_extract, "r:gz", task, callback)
                elif archive_str.endswith(".zip"):
                    self._extract_zip(archive_path, temp_extract, task, callback)
                else:
                    raise ValueError(f"Unsupported archive format: {archive_path.name}")

                # Windows stabilization delay
                if platform.system() == "Windows":
                    gc.collect()
                    time.sleep(1.0)

                # Determine source directory (handle single-subdir archives like GitHub releases)
                items = list(temp_extract.iterdir())
                if len(items) == 1 and items[0].is_dir():
                    source_dir = items[0]
                else:
                    source_dir = temp_extract

                # Move to final destination
                if dest_path.exists():
                    shutil.rmtree(dest_path, ignore_errors=True)
                shutil.move(str(source_dir), str(dest_path))

                callback.on_progress(task.name, TaskPhase.UNPACKING, 1, 1, "Extraction complete")
                return dest_path

            except PermissionError as e:
                last_error = e
                logger.warning(
                    "Extraction attempt %d/%d failed for %s (PermissionError, likely AV): %s",
                    attempt + 1,
                    _MAX_EXTRACT_RETRIES,
                    task.name,
                    e,
                )
                if attempt == _MAX_EXTRACT_RETRIES - 1:
                    raise
            except ValueError:
                # Unsupported format - don't retry
                raise
            finally:
                # Cleanup temp directory
                if temp_extract.exists():
                    shutil.rmtree(temp_extract, ignore_errors=True)

        # Should not reach here, but satisfy type checker
        raise last_error  # type: ignore[misc]

    def _extract_tar(self, archive_path: Path, dest: Path, mode: str, task: PackageTask, callback: ProgressCallback) -> None:
        """Extract a tar archive with progress reporting.

        Args:
            archive_path: Path to the tar archive.
            dest: Destination directory.
            mode: Tar open mode (e.g. "r:xz", "r:gz").
            task: Package task for callback identification.
            callback: Progress callback.
        """
        import tarfile

        with tarfile.open(archive_path, mode) as tar:  # type: ignore[call-overload]
            members = tar.getmembers()
            total = len(members)
            for i, member in enumerate(members):
                tar.extract(member, dest, filter="data")
                if (i + 1) % max(1, total // 20) == 0 or i == total - 1:
                    callback.on_progress(task.name, TaskPhase.UNPACKING, i + 1, total, f"Extracting files ({i + 1}/{total})")

    def _extract_zip(self, archive_path: Path, dest: Path, task: PackageTask, callback: ProgressCallback) -> None:
        """Extract a zip archive with progress reporting.

        Args:
            archive_path: Path to the zip archive.
            dest: Destination directory.
            task: Package task for callback identification.
            callback: Progress callback.
        """
        import zipfile

        with zipfile.ZipFile(archive_path, "r") as zf:
            members = zf.namelist()
            total = len(members)
            for i, member in enumerate(members):
                zf.extract(member, dest)
                if (i + 1) % max(1, total // 20) == 0 or i == total - 1:
                    callback.on_progress(task.name, TaskPhase.UNPACKING, i + 1, total, f"Extracting files ({i + 1}/{total})")

    def shutdown(self) -> None:
        """Shut down the thread pool, waiting for all pending extractions to finish."""
        with self._lock:
            self._shutdown = True
        self._executor.shutdown(wait=True)

    @property
    def max_workers(self) -> int:
        """Maximum number of concurrent unpack workers."""
        return self._max_workers

    def __enter__(self) -> "UnpackPool":
        return self

    def __exit__(self, exc_type: Any, exc_val: Any, exc_tb: Any) -> None:
        self.shutdown()


class InstallPool:
    """Thread pool for post-extraction package installation.

    Handles verification, fingerprinting, and finalization of extracted
    packages. CPU-bound work with no network or heavy disk I/O.

    Args:
        max_workers: Maximum concurrent install operations.
    """

    def __init__(self, max_workers: int) -> None:
        self._max_workers = max_workers
        self._executor = ThreadPoolExecutor(max_workers=max_workers, thread_name_prefix="install")
        self._shutdown = False
        self._lock = threading.Lock()

    def submit_install(self, task: PackageTask, extracted_path: Path, callback: ProgressCallback) -> Future[Path]:
        """Submit an install job for the given extracted package.

        Runs verification and fingerprinting on the extracted package
        contents.

        Args:
            task: Package task being installed.
            extracted_path: Path to the extracted package directory.
            callback: Progress callback for reporting installation progress.

        Returns:
            Future resolving to the final installation path.

        Raises:
            RuntimeError: If the pool has been shut down.
        """
        with self._lock:
            if self._shutdown:
                raise RuntimeError("InstallPool has been shut down")
        return self._executor.submit(self._do_install, task, extracted_path, callback)

    def _do_install(self, task: PackageTask, extracted_path: Path, callback: ProgressCallback) -> Path:
        """Execute the actual installation in a worker thread.

        Performs:
        1. Verification that extraction produced valid files
        2. File counting and size calculation
        3. Fingerprint generation and storage

        Args:
            task: Package task being installed.
            extracted_path: Path to the extracted package.
            callback: Progress callback.

        Returns:
            The final installation path (same as extracted_path).

        Raises:
            FileNotFoundError: If the extracted path doesn't exist.
        """
        if not extracted_path.exists():
            raise FileNotFoundError(f"Extracted path does not exist: {extracted_path}")

        callback.on_progress(task.name, TaskPhase.INSTALLING, 0, 3, "Verifying package contents...")

        # Step 1: Verify extraction produced files
        file_count = 0
        total_size = 0
        for item in extracted_path.rglob("*"):
            if item.is_file():
                file_count += 1
                try:
                    total_size += item.stat().st_size
                except OSError:
                    pass

        if file_count == 0:
            raise FileNotFoundError(f"No files found in extracted package: {extracted_path}")

        callback.on_progress(task.name, TaskPhase.INSTALLING, 1, 3, f"Found {file_count} files ({_format_size(total_size)})")

        # Step 2: Generate fingerprint
        callback.on_progress(task.name, TaskPhase.INSTALLING, 2, 3, "Generating fingerprint...")

        fingerprint_data = {
            "name": task.name,
            "version": task.version,
            "url": task.url,
            "file_count": file_count,
            "total_size": total_size,
            "installed_at": time.time(),
        }

        # Write fingerprint file
        import json

        fingerprint_path = extracted_path / ".pipeline_fingerprint.json"
        with open(fingerprint_path, "w") as f:
            json.dump(fingerprint_data, f, indent=2)

        callback.on_progress(task.name, TaskPhase.INSTALLING, 3, 3, "Installation complete")
        return extracted_path

    def shutdown(self) -> None:
        """Shut down the thread pool, waiting for all pending installs to finish."""
        with self._lock:
            self._shutdown = True
        self._executor.shutdown(wait=True)

    @property
    def max_workers(self) -> int:
        """Maximum number of concurrent install workers."""
        return self._max_workers

    def __enter__(self) -> "InstallPool":
        return self

    def __exit__(self, exc_type: Any, exc_val: Any, exc_tb: Any) -> None:
        self.shutdown()


def _cleanup_temp_file(temp_file: Path) -> None:
    """Remove a temporary download file if it exists.

    Args:
        temp_file: Path to the temporary file to remove.
    """
    try:
        if temp_file.exists():
            temp_file.unlink()
    except (PermissionError, OSError):
        pass


def _format_transfer_speed(downloaded_bytes: int, task: PackageTask) -> str:
    """Format a human-readable transfer speed string.

    Args:
        downloaded_bytes: Total bytes downloaded so far.
        task: The package task (uses start_time for rate calculation).

    Returns:
        Formatted string like "2.1 MB/s" or byte count if speed unavailable.
    """
    if task.start_time is not None:
        elapsed = time.monotonic() - task.start_time
        if elapsed > 0:
            speed = downloaded_bytes / elapsed
            return f"{_format_size(int(speed))}/s"
    return _format_size(downloaded_bytes)


def _format_size(size_bytes: int) -> str:
    """Format a byte count as a human-readable size string.

    Args:
        size_bytes: Number of bytes.

    Returns:
        Formatted string like "2.1 MB", "512 KB", etc.
    """
    if size_bytes >= 1024 * 1024 * 1024:
        return f"{size_bytes / (1024 * 1024 * 1024):.1f} GB"
    if size_bytes >= 1024 * 1024:
        return f"{size_bytes / (1024 * 1024):.1f} MB"
    if size_bytes >= 1024:
        return f"{size_bytes / 1024:.1f} KB"
    return f"{size_bytes} B"
