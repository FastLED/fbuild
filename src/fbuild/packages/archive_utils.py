"""Archive Extraction Utilities.

This module provides utilities for downloading and extracting compressed archives,
particularly for .tar.xz files used in embedded development toolchains and frameworks.
"""

import platform
from pathlib import Path
from typing import Optional

from .archive_extractors import ArchiveExtractorFactory
from .archive_strategies import (
    DirectoryMover,
    FileOperations,
    UnixRetryStrategy,
    WindowsRetryStrategy,
)
from .downloader import DownloadError, ExtractionError, PackageDownloader


class ArchiveExtractionError(Exception):
    """Raised when archive extraction operations fail."""

    pass


class ArchiveExtractor:
    """Handles downloading and extracting compressed archives.

    Supports .tar.xz, .tar.gz, and .zip archives with automatic cleanup and proper error handling.
    Uses strategy pattern to eliminate code duplication across archive formats.
    """

    def __init__(self, show_progress: bool = True):
        """Initialize archive extractor.

        Args:
            show_progress: Whether to show download/extraction progress
        """
        self.show_progress = show_progress
        self.downloader = PackageDownloader()
        self._is_windows = platform.system() == "Windows"

        # Initialize retry strategy based on platform
        retry_strategy = WindowsRetryStrategy() if self._is_windows else UnixRetryStrategy()

        # Initialize file operations and directory mover
        self.file_ops = FileOperations(retry_strategy, show_progress=show_progress)
        self.directory_mover = DirectoryMover(self.file_ops, show_progress=show_progress)

    def download_and_extract(
        self,
        url: str,
        target_dir: Path,
        description: str,
        cache_dir: Optional[Path] = None,
    ) -> None:
        """Download and extract a compressed archive.

        Supports .tar.xz, .tar.gz, and .zip archives.

        Args:
            url: URL to the archive
            target_dir: Directory to extract contents into
            description: Human-readable description for progress messages
            cache_dir: Optional directory to cache the downloaded archive
                      (defaults to parent of target_dir)

        Raises:
            DownloadError: If download fails
            ExtractionError: If extraction fails
            ArchiveExtractionError: If any other extraction operation fails
        """
        try:
            archive_name = Path(url).name
            cache_dir = cache_dir or target_dir.parent
            archive_path = cache_dir / archive_name

            # Download if not cached
            if not archive_path.exists():
                if self.show_progress:
                    print(f"Downloading {description}...")
                archive_path.parent.mkdir(parents=True, exist_ok=True)
                self.downloader.download(url, archive_path, show_progress=self.show_progress)
            else:
                if self.show_progress:
                    print(f"Using cached {description} archive")

            # Extract to target directory using factory pattern
            if self.show_progress:
                print(f"Extracting {description}...")

            extractor = ArchiveExtractorFactory.create_extractor(
                archive_path,
                self.file_ops,
                self.directory_mover,
                show_progress=self.show_progress,
                is_windows=self._is_windows,
            )

            extractor.extract(archive_path, target_dir)

        except (DownloadError, ExtractionError):
            raise
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
        except Exception as e:
            raise ArchiveExtractionError(f"Failed to extract {description}: {e}")


class URLVersionExtractor:
    """Utilities for extracting version information from URLs."""

    @staticmethod
    def extract_version_from_url(url: str, prefix: str = "") -> str:
        """Extract version string from a package URL.

        Handles common URL patterns used in GitHub releases and package repositories.

        Args:
            url: Package URL (e.g., https://github.com/.../download/3.3.4/esp32-3.3.4.tar.xz)
            prefix: Optional filename prefix to look for (e.g., "esp32-")

        Returns:
            Version string (e.g., "3.3.4")

        Examples:
            >>> URLVersionExtractor.extract_version_from_url(
            ...     "https://github.com/.../releases/download/3.3.4/esp32-3.3.4.tar.xz",
            ...     prefix="esp32-"
            ... )
            '3.3.4'
        """
        # URL format: .../releases/download/{version}/package-{version}.tar.xz
        parts = url.split("/")
        for i, part in enumerate(parts):
            if part == "download" and i + 1 < len(parts):
                version = parts[i + 1]
                # Clean up version (remove any suffixes)
                return version.split("-")[0] if "-" in version else version

        # Fallback: extract from filename
        filename = url.split("/")[-1]
        if prefix and prefix in filename:
            version_part = filename.replace(prefix, "").replace(".tar.xz", "")
            version_part = version_part.replace(".tar.gz", "")
            return version_part.split("-")[0] if "-" in version_part else version_part

        # Remove common archive extensions
        filename_no_ext = filename.replace(".tar.xz", "").replace(".tar.gz", "")
        filename_no_ext = filename_no_ext.replace(".zip", "")

        # Try to find version pattern (e.g., "1.2.3", "v1.2.3")
        import re

        version_match = re.search(r"v?(\d+\.\d+\.\d+)", filename_no_ext)
        if version_match:
            return version_match.group(1)

        # Last resort: use URL hash
        from .cache import Cache

        return Cache.hash_url(url)[:8]
