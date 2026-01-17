"""Package downloader with progress tracking and checksum verification.

This module handles downloading packages from URLs, extracting archives,
and verifying integrity with checksums.
"""

import gc
import hashlib
import platform
import tarfile
import time
import zipfile
from pathlib import Path
from typing import TYPE_CHECKING, Any, Callable, Optional, TypeVar
from urllib.parse import urlparse

if TYPE_CHECKING:
    import requests
    from tqdm import tqdm

try:
    import requests
    from tqdm import tqdm

    REQUESTS_AVAILABLE = True
except ImportError:
    REQUESTS_AVAILABLE = False
    requests: Any = None
    tqdm: Any = None


class DownloadError(Exception):
    """Raised when download fails."""

    pass


class ChecksumError(Exception):
    """Raised when checksum verification fails."""

    pass


class ExtractionError(Exception):
    """Raised when archive extraction fails."""

    pass


T = TypeVar("T")


def _retry_windows_file_operation(
    operation: Callable[[], T],
    max_retries: int = 10,
    initial_delay: float = 0.05,
) -> T:
    """Retry a file operation on Windows to handle transient locking issues.

    Windows file handles can be delayed in release due to antivirus scanning,
    delayed garbage collection, or OS-level file caching. This function retries
    file operations with exponential backoff to handle these transient issues.

    Args:
        operation: Callable that performs the file operation
        max_retries: Maximum number of retry attempts
        initial_delay: Initial delay in seconds (doubles each retry)

    Returns:
        Result of the operation

    Raises:
        The last exception encountered if all retries fail
    """
    is_windows = platform.system() == "Windows"

    if not is_windows:
        # On non-Windows systems, just call the function directly
        return operation()

    # Windows: use retry logic for file operations
    delay = initial_delay
    last_exception = None

    for attempt in range(max_retries):
        try:
            # Force garbage collection to release file handles
            if attempt > 0:
                gc.collect()
                time.sleep(delay)

            return operation()

        except (PermissionError, OSError, FileNotFoundError) as e:
            # WinError 32: File is being used by another process
            # WinError 2: File not found (temp file disappeared due to handle delays)
            # Also catch OSError with errno 13 (access denied) or 32 (in use)
            last_exception = e

            # Check if this is a retriable error
            is_retriable = False
            if isinstance(e, (PermissionError, FileNotFoundError)):
                is_retriable = True
            elif hasattr(e, "errno") and e.errno in (2, 13, 32):
                is_retriable = True

            if is_retriable and attempt < max_retries - 1:
                delay = min(delay * 2, 2.0)  # Exponential backoff, max 2s
                continue

            # Not retriable or exhausted retries
            raise

    # If we get here, all retries failed
    if last_exception:
        raise last_exception
    raise PermissionError(f"Failed to perform file operation after {max_retries} attempts")


class PackageDownloader:
    """Downloads and extracts packages with progress tracking."""

    def __init__(self, chunk_size: int = 8192):
        """Initialize downloader.

        Args:
            chunk_size: Size of chunks for downloading and hashing
        """
        self.chunk_size = chunk_size

        if not REQUESTS_AVAILABLE:
            raise ImportError("requests and tqdm are required for downloading. " + "Install with: pip install requests tqdm")

    def download(
        self,
        url: str,
        dest_path: Path,
        checksum: Optional[str] = None,
        show_progress: bool = True,
    ) -> Path:
        """Download a file from a URL.

        Args:
            url: URL to download from
            dest_path: Destination file path
            checksum: Optional SHA256 checksum for verification
            show_progress: Whether to show progress bar

        Returns:
            Path to the downloaded file

        Raises:
            DownloadError: If download fails
            ChecksumError: If checksum verification fails
        """
        dest_path = Path(dest_path)
        dest_path.parent.mkdir(parents=True, exist_ok=True)

        # Use temporary file during download
        # Use .download extension instead of .tmp to avoid antivirus interference
        temp_file = Path(str(dest_path) + ".download")

        try:
            # Start download with streaming
            response = requests.get(url, stream=True, timeout=30)
            response.raise_for_status()

            # Get file size for progress bar
            total_size = int(response.headers.get("content-length", 0))

            # Setup progress bar
            progress_bar = None
            if show_progress and total_size > 0:
                filename = Path(urlparse(url).path).name
                progress_bar = tqdm(
                    total=total_size,
                    unit="B",
                    unit_scale=True,
                    unit_divisor=1024,
                    desc=f"Downloading {filename}",
                )

            # Download file
            sha256 = hashlib.sha256() if checksum else None

            with open(temp_file, "wb") as f:
                for chunk in response.iter_content(chunk_size=self.chunk_size):
                    if chunk:
                        f.write(chunk)
                        if progress_bar:
                            progress_bar.update(len(chunk))
                        if sha256:
                            sha256.update(chunk)

            if progress_bar:
                progress_bar.close()

            # Force garbage collection to help release file handles (Windows)
            gc.collect()

            # On Windows, add delay to let file handles stabilize after write
            if platform.system() == "Windows":
                time.sleep(0.2)

                # Check if temp file still exists (antivirus might quarantine immediately)
                if not temp_file.exists():
                    # File was quarantined immediately after download
                    # Check if antivirus moved it to dest_path already
                    if dest_path.exists():
                        # Antivirus renamed it for us
                        return dest_path
                    # Otherwise, file was deleted/quarantined - this is unrecoverable
                    raise DownloadError(
                        f"Downloaded file was immediately quarantined by antivirus: {temp_file}. " + f"Try adding an exclusion for {dest_path.parent} or disabling antivirus temporarily."
                    )

            # Verify checksum if provided
            if checksum and sha256:
                actual_checksum = sha256.hexdigest()
                if actual_checksum.lower() != checksum.lower():
                    # Delete temp file before raising (not inside retry wrapper)
                    try:
                        _retry_windows_file_operation(lambda: temp_file.unlink())
                    except KeyboardInterrupt as ke:
                        from fbuild.interrupt_utils import (
                            handle_keyboard_interrupt_properly,
                        )

                        handle_keyboard_interrupt_properly(ke)
                    except Exception:
                        pass  # Ignore unlink errors, we're about to raise checksum error anyway
                    # Raise checksum error (NOT inside retry wrapper)
                    raise ChecksumError(f"Checksum mismatch for {url}\n" + f"Expected: {checksum}\n" + f"Got: {actual_checksum}")

            # Move temp file to final destination
            # For large files, antivirus scanning can take longer, so use more aggressive retry
            if dest_path.exists():
                # Try to delete existing file, but don't fail if antivirus is holding it
                # The rename logic below will handle this case with extended retry
                try:
                    _retry_windows_file_operation(lambda: dest_path.unlink())
                except (PermissionError, OSError):
                    # File is locked (likely by antivirus), skip deletion
                    # The rename will handle this with copy fallback
                    pass

            # Use longer max delay for rename to handle antivirus scanning of large files
            def rename_with_extended_retry() -> Path:
                """Rename with extended retry specifically for large file antivirus delays."""
                import shutil

                delay = 0.2
                max_delay = 15.0  # Allow up to 15s for antivirus scanning

                # Initial check - fail fast if file is already gone
                if not temp_file.exists():
                    if dest_path.exists():
                        # Antivirus already moved it
                        return dest_path
                    raise FileNotFoundError(f"Temp file was quarantined immediately after download: {temp_file}. " + f"Add antivirus exclusion for {dest_path.parent} and retry.")

                for attempt in range(30):  # More attempts for rename
                    try:
                        if attempt > 0:
                            gc.collect()
                            time.sleep(delay)

                        # Check if temp file still exists (antivirus might have moved/quarantined it)
                        if not temp_file.exists():
                            # Wait and check if it reappears or if dest already exists
                            time.sleep(min(delay * 2, max_delay))
                            gc.collect()  # Try to release any file handles

                            if dest_path.exists():
                                # File was already moved (possibly by antivirus restoration)
                                return dest_path

                            # Check again after longer wait
                            if not temp_file.exists():
                                # File disappeared - could be antivirus quarantine
                                if attempt < 25:  # Give more attempts for file to reappear
                                    delay = min(delay * 1.3, max_delay)
                                    continue

                                # Last resort: check if dest_path appeared
                                if dest_path.exists():
                                    return dest_path

                                raise FileNotFoundError(
                                    f"Temp file disappeared (possibly quarantined by antivirus): {temp_file}. " + f"Try disabling antivirus or adding an exclusion for {dest_path.parent}"
                                )

                        # Try to rename/move the file
                        try:
                            # On Windows, if dest exists, try to delete it first
                            if dest_path.exists():
                                try:
                                    dest_path.unlink()
                                except (PermissionError, OSError):
                                    # Dest file is locked, use copy fallback immediately
                                    if attempt >= 3:  # Give a few attempts, then use copy
                                        shutil.copy2(temp_file, dest_path)
                                        try:
                                            temp_file.unlink()
                                        except KeyboardInterrupt as ke:
                                            from fbuild.interrupt_utils import (
                                                handle_keyboard_interrupt_properly,
                                            )

                                            handle_keyboard_interrupt_properly(ke)
                                            raise  # Never reached, but satisfies type checker
                                        except Exception:
                                            pass  # Ignore unlink errors after successful copy
                                        return dest_path
                                    # Otherwise retry
                                    raise

                            return temp_file.rename(dest_path)
                        except (PermissionError, OSError) as rename_err:
                            # If rename fails, try copy + delete as fallback
                            if attempt >= 15:  # Use copy method after multiple rename failures
                                try:
                                    shutil.copy2(temp_file, dest_path)
                                    try:
                                        temp_file.unlink()
                                    except KeyboardInterrupt as ke:
                                        from fbuild.interrupt_utils import (
                                            handle_keyboard_interrupt_properly,
                                        )

                                        handle_keyboard_interrupt_properly(ke)
                                        raise  # Never reached, but satisfies type checker
                                    except Exception:
                                        pass  # Ignore unlink errors after successful copy
                                    return dest_path
                                except KeyboardInterrupt as ke:
                                    from fbuild.interrupt_utils import (
                                        handle_keyboard_interrupt_properly,
                                    )

                                    handle_keyboard_interrupt_properly(ke)
                                    raise  # Never reached, but satisfies type checker
                                except Exception:
                                    pass  # Let the outer exception handling deal with it
                            raise rename_err

                    except (PermissionError, OSError, FileNotFoundError) as e:
                        if attempt < 29 and (isinstance(e, (PermissionError, FileNotFoundError)) or (hasattr(e, "errno") and e.errno in (2, 13, 32))):
                            delay = min(delay * 1.3, max_delay)
                            continue
                        raise
                raise PermissionError(f"Failed to move file after extended retry: {temp_file} -> {dest_path}")

            if platform.system() == "Windows":
                dest_path = rename_with_extended_retry()
            else:
                dest_path = _retry_windows_file_operation(lambda: temp_file.rename(dest_path))

            return dest_path

        except requests.RequestException as e:
            if temp_file.exists():
                try:
                    _retry_windows_file_operation(lambda: temp_file.unlink())
                except KeyboardInterrupt as ke:
                    from fbuild.interrupt_utils import (
                        handle_keyboard_interrupt_properly,
                    )

                    handle_keyboard_interrupt_properly(ke)
                    raise  # Never reached, but satisfies type checker
                except Exception:
                    pass  # Ignore cleanup errors when reporting download error
            raise DownloadError(f"Failed to download {url}: {e}")

        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception:
            if temp_file.exists():
                try:
                    _retry_windows_file_operation(lambda: temp_file.unlink())
                except KeyboardInterrupt as ke:
                    from fbuild.interrupt_utils import (
                        handle_keyboard_interrupt_properly,
                    )

                    handle_keyboard_interrupt_properly(ke)
                    raise  # Never reached, but satisfies type checker
                except Exception:
                    pass  # Ignore cleanup errors when reporting original error
            raise

    def extract_archive(self, archive_path: Path, dest_dir: Path, show_progress: bool = True) -> Path:
        """Extract an archive file.

        Supports .tar.gz, .tar.bz2, .tar.xz, and .zip formats.

        Args:
            archive_path: Path to the archive file
            dest_dir: Destination directory for extraction
            show_progress: Whether to show progress information

        Returns:
            Path to the extracted directory

        Raises:
            ExtractionError: If extraction fails
        """
        archive_path = Path(archive_path)
        dest_dir = Path(dest_dir)

        if not archive_path.exists():
            raise ExtractionError(f"Archive not found: {archive_path}")

        dest_dir.mkdir(parents=True, exist_ok=True)

        try:
            if show_progress:
                print(f"Extracting {archive_path.name}...")

            # Determine archive type and extract
            if archive_path.suffix == ".zip":
                self._extract_zip(archive_path, dest_dir)
            elif archive_path.name.endswith((".tar.gz", ".tar.bz2", ".tar.xz")):
                self._extract_tar(archive_path, dest_dir)
            else:
                raise ExtractionError(f"Unsupported archive format: {archive_path.suffix}")

            return dest_dir

        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise ExtractionError(f"Failed to extract {archive_path}: {e}")

    def _extract_tar(self, archive_path: Path, dest_dir: Path) -> None:
        """Extract a tar archive.

        Args:
            archive_path: Path to tar archive
            dest_dir: Destination directory
        """
        with tarfile.open(archive_path, "r:*") as tar:
            tar.extractall(dest_dir)

    def _extract_zip(self, archive_path: Path, dest_dir: Path) -> None:
        """Extract a zip archive.

        Args:
            archive_path: Path to zip archive
            dest_dir: Destination directory
        """
        with zipfile.ZipFile(archive_path, "r") as zip_file:
            zip_file.extractall(dest_dir)

    def download_and_extract(
        self,
        url: str,
        cache_dir: Path,
        extract_dir: Path,
        checksum: Optional[str] = None,
        show_progress: bool = True,
    ) -> Path:
        """Download and extract a package in one operation.

        Args:
            url: URL to download from
            cache_dir: Directory to cache the downloaded archive
            extract_dir: Directory to extract to
            checksum: Optional SHA256 checksum
            show_progress: Whether to show progress

        Returns:
            Path to the extracted directory
        """
        # Determine archive filename from URL
        filename = Path(urlparse(url).path).name
        archive_path = cache_dir / filename

        # Download if not cached
        if not archive_path.exists():
            self.download(url, archive_path, checksum, show_progress)
        elif show_progress:
            print(f"Using cached {filename}")

        # Extract
        return self.extract_archive(archive_path, extract_dir, show_progress)

    def verify_checksum(self, file_path: Path, expected: str) -> bool:
        """Verify SHA256 checksum of a file.

        Args:
            file_path: Path to file to verify
            expected: Expected SHA256 checksum (hex string)

        Returns:
            True if checksum matches

        Raises:
            ChecksumError: If checksum doesn't match
        """
        sha256 = hashlib.sha256()

        with open(file_path, "rb") as f:
            for chunk in iter(lambda: f.read(self.chunk_size), b""):
                sha256.update(chunk)

        actual = sha256.hexdigest()
        if actual.lower() != expected.lower():
            raise ChecksumError(f"Checksum mismatch for {file_path}\n" + f"Expected: {expected}\n" + f"Got: {actual}")

        return True
