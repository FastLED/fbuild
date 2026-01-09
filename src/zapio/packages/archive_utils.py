"""Archive Extraction Utilities.

This module provides utilities for downloading and extracting compressed archives,
particularly for .tar.xz files used in embedded development toolchains and frameworks.
"""

import shutil
import tarfile
from pathlib import Path
from typing import Optional

from .downloader import DownloadError, ExtractionError, PackageDownloader


class ArchiveExtractionError(Exception):
    """Raised when archive extraction operations fail."""

    pass


class ArchiveExtractor:
    """Handles downloading and extracting compressed archives.

    Supports .tar.xz archives with automatic cleanup and proper error handling.
    """

    def __init__(self, show_progress: bool = True):
        """Initialize archive extractor.

        Args:
            show_progress: Whether to show download/extraction progress
        """
        self.show_progress = show_progress
        self.downloader = PackageDownloader()

    def download_and_extract(
        self,
        url: str,
        target_dir: Path,
        description: str,
        cache_dir: Optional[Path] = None,
    ) -> None:
        """Download and extract a .tar.xz archive.

        Args:
            url: URL to the .tar.xz archive
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
                self.downloader.download(
                    url, archive_path, show_progress=self.show_progress
                )
            else:
                if self.show_progress:
                    print(f"Using cached {description} archive")

            # Extract to target directory
            if self.show_progress:
                print(f"Extracting {description}...")

            # Detect archive type and use appropriate extraction method
            if archive_path.suffix == ".zip":
                self._extract_zip(archive_path, target_dir)
            elif archive_path.name.endswith(".tar.xz") or archive_path.name.endswith(
                ".txz"
            ):
                self._extract_tar_xz(archive_path, target_dir)
            elif archive_path.name.endswith((".tar.gz", ".tgz")):
                self._extract_tar_gz(archive_path, target_dir)
            else:
                # Default to tar.xz for backwards compatibility
                self._extract_tar_xz(archive_path, target_dir)

        except (DownloadError, ExtractionError):
            raise
        except KeyboardInterrupt as ke:
            from zapio.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
        except Exception as e:
            raise ArchiveExtractionError(f"Failed to extract {description}: {e}")

    def _extract_tar_xz(self, archive_path: Path, target_dir: Path) -> None:
        """Extract a .tar.xz archive to target directory.

        Handles archives that extract to a single subdirectory or directly to multiple files.

        Args:
            archive_path: Path to the .tar.xz archive file
            target_dir: Directory to extract contents into

        Raises:
            ExtractionError: If extraction fails
        """
        # Create temp extraction directory
        temp_extract = target_dir.parent / f"temp_extract_{archive_path.name}"
        temp_extract.mkdir(parents=True, exist_ok=True)

        try:
            # Extract .tar.xz archive
            with tarfile.open(archive_path, "r:xz") as tar:
                tar.extractall(temp_extract)

            # Find the extracted directory
            # Usually it's a subdirectory like "esp32/" or directly extracted
            extracted_items = list(temp_extract.iterdir())

            if len(extracted_items) == 1 and extracted_items[0].is_dir():
                # Single directory extracted - use its contents
                source_dir = extracted_items[0]
            else:
                # Multiple items or files - use temp_extract as source
                source_dir = temp_extract

            # Move contents to target directory
            target_dir.mkdir(parents=True, exist_ok=True)

            for item in source_dir.iterdir():
                dest = target_dir / item.name
                if item.is_dir():
                    if dest.exists():
                        shutil.rmtree(dest)
                    shutil.copytree(item, dest)
                else:
                    if dest.exists():
                        dest.unlink()
                    shutil.copy2(item, dest)

        except KeyboardInterrupt as ke:
            from zapio.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
        except Exception as e:
            raise ExtractionError(f"Failed to extract archive: {e}")
        finally:
            # Clean up temp directory
            if temp_extract.exists():
                shutil.rmtree(temp_extract, ignore_errors=True)

    def _extract_zip(self, archive_path: Path, target_dir: Path) -> None:
        """Extract a .zip archive to target directory.

        Handles archives that extract to a single subdirectory or directly to multiple files.

        Args:
            archive_path: Path to the .zip archive file
            target_dir: Directory to extract contents into

        Raises:
            ExtractionError: If extraction fails
        """
        import zipfile

        # Create temp extraction directory
        temp_extract = target_dir.parent / f"temp_extract_{archive_path.name}"
        temp_extract.mkdir(parents=True, exist_ok=True)

        try:
            # Extract .zip archive
            with zipfile.ZipFile(archive_path, "r") as zf:
                zf.extractall(temp_extract)

            # Find the extracted directory
            extracted_items = list(temp_extract.iterdir())

            if len(extracted_items) == 1 and extracted_items[0].is_dir():
                # Single directory extracted - use its contents
                source_dir = extracted_items[0]
            else:
                # Multiple items or files - use temp_extract as source
                source_dir = temp_extract

            # Move contents to target directory
            target_dir.mkdir(parents=True, exist_ok=True)

            for item in source_dir.iterdir():
                dest = target_dir / item.name
                if item.is_dir():
                    if dest.exists():
                        shutil.rmtree(dest)
                    shutil.copytree(item, dest)
                else:
                    if dest.exists():
                        dest.unlink()
                    shutil.copy2(item, dest)

        except KeyboardInterrupt as ke:
            from zapio.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
        except Exception as e:
            raise ExtractionError(f"Failed to extract archive: {e}")
        finally:
            # Clean up temp directory
            if temp_extract.exists():
                shutil.rmtree(temp_extract, ignore_errors=True)

    def _extract_tar_gz(self, archive_path: Path, target_dir: Path) -> None:
        """Extract a .tar.gz archive to target directory.

        Handles archives that extract to a single subdirectory or directly to multiple files.

        Args:
            archive_path: Path to the .tar.gz archive file
            target_dir: Directory to extract contents into

        Raises:
            ExtractionError: If extraction fails
        """
        # Create temp extraction directory
        temp_extract = target_dir.parent / f"temp_extract_{archive_path.name}"
        temp_extract.mkdir(parents=True, exist_ok=True)

        try:
            # Extract .tar.gz archive
            with tarfile.open(archive_path, "r:gz") as tar:
                tar.extractall(temp_extract)

            # Find the extracted directory
            extracted_items = list(temp_extract.iterdir())

            if len(extracted_items) == 1 and extracted_items[0].is_dir():
                # Single directory extracted - use its contents
                source_dir = extracted_items[0]
            else:
                # Multiple items or files - use temp_extract as source
                source_dir = temp_extract

            # Move contents to target directory
            target_dir.mkdir(parents=True, exist_ok=True)

            for item in source_dir.iterdir():
                dest = target_dir / item.name
                if item.is_dir():
                    if dest.exists():
                        shutil.rmtree(dest)
                    shutil.copytree(item, dest)
                else:
                    if dest.exists():
                        dest.unlink()
                    shutil.copy2(item, dest)

        except KeyboardInterrupt as ke:
            from zapio.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
        except Exception as e:
            raise ExtractionError(f"Failed to extract archive: {e}")
        finally:
            # Clean up temp directory
            if temp_extract.exists():
                shutil.rmtree(temp_extract, ignore_errors=True)


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
