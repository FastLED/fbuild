"""Concurrent package download and installation manager.

This module provides thread-safe package management with:
- Fine-grained per-package locking
- Parallel downloads via ThreadPoolExecutor
- Atomic installation with rollback
- Fingerprint-based validation
"""

import logging
import shutil
import tempfile
import threading
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Dict, Iterator, List, Optional

from .cache import Cache
from .downloader import DownloadError, ExtractionError, PackageDownloader
from .fingerprint import FingerprintRegistry, PackageFingerprint

logger = logging.getLogger(__name__)

# Default lock timeout: 10 minutes (for large package downloads)
DEFAULT_PACKAGE_LOCK_TIMEOUT = 600.0


@dataclass
class PackageSpec:
    """Specification for a package to download/install."""

    name: str
    url: str
    version: str = "latest"
    checksum: Optional[str] = None
    key_files: Optional[List[str]] = None  # Files to verify after extraction
    post_install: Optional[Callable[[Path], None]] = None  # Post-install hook


@dataclass
class PackageResult:
    """Result of a package installation."""

    spec: PackageSpec
    success: bool
    install_path: Optional[Path]
    fingerprint: Optional[PackageFingerprint]
    error: Optional[str]
    elapsed_time: float
    was_cached: bool


class PackageLockError(RuntimeError):
    """Error raised when a package lock cannot be acquired."""

    def __init__(self, package_id: str, message: str):
        self.package_id = package_id
        super().__init__(f"Package lock error for '{package_id}': {message}")


@dataclass
class PackageLockInfo:
    """Information about a package lock."""

    lock: threading.Lock
    created_at: float
    acquired_at: Optional[float] = None
    holder_thread_id: Optional[int] = None
    holder_description: Optional[str] = None

    def is_held(self) -> bool:
        """Check if lock is currently held."""
        return self.acquired_at is not None


class ConcurrentPackageManager:
    """Thread-safe package download and extraction manager.

    Provides concurrent package management with:
    - Per-package fine-grained locking (no blocking unrelated packages)
    - Parallel downloads using ThreadPoolExecutor
    - Atomic extraction with rollback on failure
    - Fingerprint-based cache validation

    Example:
        >>> manager = ConcurrentPackageManager(cache)
        >>> specs = [
        ...     PackageSpec(name="toolchain", url="https://..."),
        ...     PackageSpec(name="framework", url="https://..."),
        ... ]
        >>> results = manager.ensure_packages(specs)
        >>> for result in results:
        ...     if result.success:
        ...         print(f"{result.spec.name}: {result.install_path}")
    """

    def __init__(
        self,
        cache: Cache,
        max_workers: int = 4,
        show_progress: bool = True,
    ):
        """Initialize concurrent package manager.

        Args:
            cache: Cache instance for storing packages
            max_workers: Maximum number of concurrent downloads
            show_progress: Whether to show download progress
        """
        self.cache = cache
        self.max_workers = max_workers
        self.show_progress = show_progress
        self.downloader = PackageDownloader()
        self.registry = FingerprintRegistry(cache.cache_root)

        # Locking infrastructure
        self._master_lock = threading.Lock()
        self._package_locks: Dict[str, PackageLockInfo] = {}

    def _get_package_id(self, url: str, version: str) -> str:
        """Generate unique package identifier."""
        url_hash = PackageFingerprint.hash_url(url)
        return f"{url_hash}:{version}"

    def _get_or_create_lock(self, package_id: str) -> PackageLockInfo:
        """Get or create a lock for the given package."""
        with self._master_lock:
            if package_id not in self._package_locks:
                self._package_locks[package_id] = PackageLockInfo(
                    lock=threading.Lock(),
                    created_at=time.time(),
                )
            return self._package_locks[package_id]

    @contextmanager
    def acquire_package_lock(
        self,
        url: str,
        version: str,
        blocking: bool = True,
        timeout: float = DEFAULT_PACKAGE_LOCK_TIMEOUT,
        description: Optional[str] = None,
    ) -> Iterator[None]:
        """Acquire a lock for a specific package.

        This ensures that only one thread can download/extract a package
        at a time, while allowing other packages to be processed concurrently.

        Args:
            url: Package URL
            version: Package version
            blocking: If True, wait for lock. If False, raise error if unavailable.
            timeout: Maximum time to wait for lock
            description: Human-readable description of operation

        Yields:
            None (lock is held for duration of context)

        Raises:
            PackageLockError: If lock cannot be acquired
        """
        package_id = self._get_package_id(url, version)
        lock_info = self._get_or_create_lock(package_id)

        logger.debug(f"Acquiring package lock for: {package_id}")

        acquired = lock_info.lock.acquire(blocking=blocking, timeout=timeout if blocking else -1)
        if not acquired:
            raise PackageLockError(package_id, f"Lock unavailable (held by: {lock_info.holder_description or 'unknown'})")

        try:
            with self._master_lock:
                lock_info.acquired_at = time.time()
                lock_info.holder_thread_id = threading.get_ident()
                lock_info.holder_description = description or f"Package operation for {package_id}"

            logger.debug(f"Package lock acquired for: {package_id}")
            yield
        finally:
            with self._master_lock:
                lock_info.acquired_at = None
                lock_info.holder_thread_id = None
                lock_info.holder_description = None
            lock_info.lock.release()
            logger.debug(f"Package lock released for: {package_id}")

    def is_package_installed(self, url: str, version: str) -> bool:
        """Check if a package is already installed and valid.

        Args:
            url: Package URL
            version: Package version

        Returns:
            True if package is installed and fingerprint valid
        """
        return self.registry.is_installed(url, version)

    def get_install_path(self, url: str, version: str) -> Optional[Path]:
        """Get installation path for a package.

        Args:
            url: Package URL
            version: Package version

        Returns:
            Installation path or None if not installed
        """
        return self.registry.get_install_path(url, version)

    def download_package(
        self,
        spec: PackageSpec,
        force: bool = False,
    ) -> PackageResult:
        """Download and install a single package (thread-safe).

        Args:
            spec: Package specification
            force: Force re-download even if cached

        Returns:
            PackageResult with installation details
        """
        start_time = time.time()

        # Check if already installed
        if not force and self.is_package_installed(spec.url, spec.version):
            install_path = self.get_install_path(spec.url, spec.version)
            fingerprint = self.registry.get_fingerprint(spec.url, spec.version)
            return PackageResult(
                spec=spec,
                success=True,
                install_path=install_path,
                fingerprint=fingerprint,
                error=None,
                elapsed_time=time.time() - start_time,
                was_cached=True,
            )

        # Acquire lock and download
        try:
            with self.acquire_package_lock(spec.url, spec.version, description=f"Download {spec.name}"):
                # Double-check after acquiring lock (another thread may have installed it)
                if not force and self.is_package_installed(spec.url, spec.version):
                    install_path = self.get_install_path(spec.url, spec.version)
                    fingerprint = self.registry.get_fingerprint(spec.url, spec.version)
                    return PackageResult(
                        spec=spec,
                        success=True,
                        install_path=install_path,
                        fingerprint=fingerprint,
                        error=None,
                        elapsed_time=time.time() - start_time,
                        was_cached=True,
                    )

                # Perform download and installation
                return self._do_download_and_install(spec, start_time, force)

        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
        except PackageLockError as e:
            return PackageResult(
                spec=spec,
                success=False,
                install_path=None,
                fingerprint=None,
                error=str(e),
                elapsed_time=time.time() - start_time,
                was_cached=False,
            )
        except Exception as e:
            return PackageResult(
                spec=spec,
                success=False,
                install_path=None,
                fingerprint=None,
                error=f"Unexpected error: {e}",
                elapsed_time=time.time() - start_time,
                was_cached=False,
            )

    def _do_download_and_install(
        self,
        spec: PackageSpec,
        start_time: float,
        force: bool,
    ) -> PackageResult:
        """Internal: perform actual download and installation.

        Must be called while holding the package lock.
        """
        url_hash = PackageFingerprint.hash_url(spec.url)

        # Determine paths
        cache_dir = self.cache.packages_dir / url_hash / spec.version
        cache_dir.mkdir(parents=True, exist_ok=True)

        # Get archive filename from URL
        archive_name = Path(spec.url.split("/")[-1].split("?")[0])
        archive_path = cache_dir / archive_name

        # Determine install directory
        install_dir = self.cache.toolchains_dir / url_hash / spec.version

        try:
            # Step 1: Download archive
            if force or not archive_path.exists():
                if self.show_progress:
                    print(f"Downloading {spec.name}...")
                self.downloader.download(
                    spec.url,
                    archive_path,
                    checksum=spec.checksum,
                    show_progress=self.show_progress,
                )
            else:
                if self.show_progress:
                    print(f"Using cached archive for {spec.name}")

            # Step 2: Atomic extraction
            self._atomic_extract(archive_path, install_dir, spec)

            # Step 3: Create fingerprint
            fingerprint = PackageFingerprint.from_archive(
                url=spec.url,
                version=spec.version,
                archive_path=archive_path,
                extracted_dir=install_dir,
                key_files=spec.key_files,
            )

            # Step 4: Save fingerprint and register
            fingerprint.save(install_dir)
            self.registry.register(fingerprint, install_dir)

            # Step 5: Run post-install hook if provided
            if spec.post_install:
                spec.post_install(install_dir)

            return PackageResult(
                spec=spec,
                success=True,
                install_path=install_dir,
                fingerprint=fingerprint,
                error=None,
                elapsed_time=time.time() - start_time,
                was_cached=False,
            )

        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
        except (DownloadError, ExtractionError) as e:
            return PackageResult(
                spec=spec,
                success=False,
                install_path=None,
                fingerprint=None,
                error=str(e),
                elapsed_time=time.time() - start_time,
                was_cached=False,
            )
        except Exception as e:
            return PackageResult(
                spec=spec,
                success=False,
                install_path=None,
                fingerprint=None,
                error=f"Installation failed: {e}",
                elapsed_time=time.time() - start_time,
                was_cached=False,
            )

    def _atomic_extract(
        self,
        archive_path: Path,
        dest_dir: Path,
        spec: PackageSpec,
    ) -> Path:
        """Extract archive atomically with rollback on failure.

        Args:
            archive_path: Path to archive file
            dest_dir: Final destination directory
            spec: Package specification

        Returns:
            Path to extracted directory

        Raises:
            ExtractionError: If extraction fails
        """
        if self.show_progress:
            print(f"Extracting {spec.name}...")

        # Extract to temporary directory first
        with tempfile.TemporaryDirectory() as temp_dir:
            temp_path = Path(temp_dir)
            self.downloader.extract_archive(archive_path, temp_path, show_progress=False)

            # Find extracted content (may be nested in a single directory)
            extracted_items = list(temp_path.iterdir())
            if len(extracted_items) == 1 and extracted_items[0].is_dir():
                source_dir = extracted_items[0]
            else:
                source_dir = temp_path

            # Remove existing destination if present
            if dest_dir.exists():
                shutil.rmtree(dest_dir)

            # Move to final location atomically
            dest_dir.parent.mkdir(parents=True, exist_ok=True)
            shutil.move(str(source_dir), str(dest_dir))

        return dest_dir

    def ensure_packages(
        self,
        specs: List[PackageSpec],
        force: bool = False,
    ) -> List[PackageResult]:
        """Download and install multiple packages in parallel.

        Args:
            specs: List of package specifications
            force: Force re-download even if cached

        Returns:
            List of PackageResult for each spec (in same order)
        """
        if not specs:
            return []

        results: Dict[str, PackageResult] = {}

        # Use ThreadPoolExecutor for parallel downloads
        with ThreadPoolExecutor(max_workers=min(self.max_workers, len(specs))) as executor:
            # Submit all download tasks
            future_to_spec = {executor.submit(self.download_package, spec, force): spec for spec in specs}

            # Collect results as they complete
            for future in as_completed(future_to_spec):
                spec = future_to_spec[future]
                try:
                    result = future.result()
                    results[spec.name] = result

                    if self.show_progress:
                        status = "✓" if result.success else "✗"
                        cached = " (cached)" if result.was_cached else ""
                        print(f"  {status} {spec.name}{cached}")

                except KeyboardInterrupt as ke:
                    from fbuild.interrupt_utils import (
                        handle_keyboard_interrupt_properly,
                    )

                    handle_keyboard_interrupt_properly(ke)
                except Exception as e:
                    results[spec.name] = PackageResult(
                        spec=spec,
                        success=False,
                        install_path=None,
                        fingerprint=None,
                        error=str(e),
                        elapsed_time=0.0,
                        was_cached=False,
                    )

        # Return results in original order
        return [results[spec.name] for spec in specs]

    def cleanup_locks(self) -> int:
        """Remove unused package locks.

        Returns:
            Number of locks removed
        """
        with self._master_lock:
            keys_to_remove = [key for key, info in self._package_locks.items() if not info.is_held()]
            for key in keys_to_remove:
                del self._package_locks[key]
            return len(keys_to_remove)

    def get_lock_status(self) -> Dict[str, Dict[str, Any]]:
        """Get current lock status for debugging.

        Returns:
            Dictionary of package_id -> lock info
        """
        with self._master_lock:
            return {
                pkg_id: {
                    "is_held": info.is_held(),
                    "holder_thread_id": info.holder_thread_id,
                    "holder_description": info.holder_description,
                    "created_at": info.created_at,
                    "acquired_at": info.acquired_at,
                }
                for pkg_id, info in self._package_locks.items()
            }
