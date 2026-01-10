"""Archive Extraction Utilities.

This module provides utilities for downloading and extracting compressed archives,
particularly for .tar.xz files used in embedded development toolchains and frameworks.
"""

import gc
import platform
import shutil
import tarfile
import time
from pathlib import Path
from typing import Any, Callable, Optional

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
        self._is_windows = platform.system() == "Windows"

    def _retry_file_operation(self, operation: Callable[..., Any], *args: Any, max_retries: int = 5, **kwargs: Any) -> Any:
        """Retry a file operation with exponential backoff on Windows.

        On Windows, file operations can fail with PermissionError, OSError,
        or FileNotFoundError due to file handle delays. This function retries
        the operation with exponential backoff.

        Args:
            operation: Function to call (e.g., Path.unlink, shutil.rmtree)
            *args: Positional arguments for the operation
            max_retries: Maximum number of retry attempts
            **kwargs: Keyword arguments for the operation

        Raises:
            The last exception if all retries fail
        """
        if not self._is_windows:
            # No retry overhead on non-Windows platforms
            return operation(*args, **kwargs)

        delay = 0.05  # Start with 50ms
        last_error = None

        for attempt in range(max_retries):
            try:
                if attempt > 0:
                    gc.collect()  # Force garbage collection
                    time.sleep(delay)
                    if self.show_progress:
                        print(f"  [Windows] Retrying file operation (attempt {attempt + 1}/{max_retries})...")

                return operation(*args, **kwargs)

            except (PermissionError, OSError, FileNotFoundError) as e:
                last_error = e
                if attempt < max_retries - 1:
                    delay = min(delay * 2, 2.0)  # Exponential backoff, max 2s
                    continue
                else:
                    # Last attempt failed
                    raise

        # Should not reach here, but just in case
        if last_error:
            raise last_error

    def _copytree_with_retry(self, src: Path, dst: Path) -> None:
        """Recursively copy directory tree with retry logic for each operation.

        Unlike shutil.copytree, this retries each individual file/directory operation,
        which is more robust on Windows where file handles may not be immediately available.

        Args:
            src: Source directory path
            dst: Destination directory path
        """
        # Create destination directory with retry
        self._retry_file_operation(dst.mkdir, parents=True, exist_ok=True)

        # Iterate over source items - wrap iterdir() in retry logic for Windows
        # On Windows, directory handles may not be immediately available after extraction
        def get_items():
            return list(src.iterdir())

        items = self._retry_file_operation(get_items)
        assert items is not None

        for item in items:
            src_item = item
            dst_item = dst / item.name

            # Check if item is a directory - wrap in retry logic
            def is_directory():
                return src_item.is_dir()

            is_dir = self._retry_file_operation(is_directory)

            if is_dir:
                # Recursively copy subdirectory
                self._copytree_with_retry(src_item, dst_item)
            else:
                # Copy file with retry
                self._retry_file_operation(shutil.copy2, src_item, dst_item)

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
                self.downloader.download(url, archive_path, show_progress=self.show_progress)
            else:
                if self.show_progress:
                    print(f"Using cached {description} archive")

            # Extract to target directory
            if self.show_progress:
                print(f"Extracting {description}...")

            # Detect archive type and use appropriate extraction method
            if archive_path.suffix == ".zip":
                self._extract_zip(archive_path, target_dir)
            elif archive_path.name.endswith(".tar.xz") or archive_path.name.endswith(".txz"):
                self._extract_tar_xz(archive_path, target_dir)
            elif archive_path.name.endswith((".tar.gz", ".tgz")):
                self._extract_tar_gz(archive_path, target_dir)
            else:
                # Default to tar.xz for backwards compatibility
                self._extract_tar_xz(archive_path, target_dir)

        except (DownloadError, ExtractionError):
            raise
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

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

        # Verify temp directory was created
        if not temp_extract.exists():
            raise ExtractionError(f"Failed to create temp extraction directory: {temp_extract}")

        try:
            # Extract .tar.xz archive with progress tracking
            with tarfile.open(archive_path, "r:xz") as tar:
                members = tar.getmembers()
                total_members = len(members)

                if self.show_progress:
                    from tqdm import tqdm

                    with tqdm(total=total_members, unit="file", desc=f"Extracting {archive_path.name}") as pbar:
                        for member in members:
                            # On Windows, individual file extractions can hit Permission Errors
                            # due to file handle delays or antivirus/Windows Defender scanning
                            # Wrap each extract in retry logic
                            if self._is_windows:

                                def extract_member():
                                    tar.extract(member, temp_extract)

                                self._retry_file_operation(extract_member, max_retries=5)
                            else:
                                tar.extract(member, temp_extract)
                            pbar.update(1)
                else:
                    tar.extractall(temp_extract)

            # On Windows, force garbage collection and add LONG delay to let file handles close
            # Large archives (3000+ files) need extensive time for Windows to stabilize
            if self._is_windows:
                gc.collect()
                time.sleep(5.0)  # Increased to 5s - filesystem stabilization for large archives

            # NOTE: Do NOT verify temp_extract.exists() on Windows!
            # Path.exists() is unreliable on Windows immediately after extraction
            # due to file handle delays - it can return False even when directory was created
            # If tar.extractall() didn't raise an exception, the extraction succeeded

            # Find the extracted directory
            # Usually it's a subdirectory like "esp32/" or directly extracted
            # Wrap in retry logic - Windows may not have the directory handle ready yet
            def get_extracted_items():
                # On Windows, the directory might not be accessible immediately after creation
                # Even after 5s delay, iterdir() can fail with WinError 3
                # Retry with increasing delays to give Windows time to stabilize
                return list(temp_extract.iterdir())

            extracted_items = self._retry_file_operation(get_extracted_items, max_retries=10) if self._is_windows else list(temp_extract.iterdir())
            assert extracted_items is not None

            # Check if single item is a directory - wrap in retry logic for Windows
            single_subdir = None
            if len(extracted_items) == 1:
                if self._is_windows:

                    def check_is_dir():
                        return extracted_items[0].is_dir()

                    is_single_dir = self._retry_file_operation(check_is_dir)
                else:
                    is_single_dir = extracted_items[0].is_dir()

                if is_single_dir:
                    # Single directory extracted - we'll move it atomically
                    single_subdir = extracted_items[0]

            # On Windows, add another delay before move operation
            if self._is_windows:
                time.sleep(1.0)  # Additional delay for directory handles

            # Move directory using shutil.move() for entire tree (atomic operation)
            # This is MUCH more reliable on Windows than iterating through individual files
            # Single atomic operation instead of 3000+ individual file operations
            if self.show_progress:
                print(f"Moving extracted files to {target_dir.name}...")

            # Track whether we need to remove target before move
            target_removal_failed = False

            # Remove existing target directory if it exists
            if target_dir.exists():
                if self.show_progress:
                    print(f"  Removing existing {target_dir.name}...")

                # On Windows, prepare for directory removal
                if self._is_windows:
                    gc.collect()  # Force garbage collection to release handles
                    time.sleep(1.0)  # Give Windows time to release handles

                try:
                    # Remove ignore_errors - let retry logic handle errors
                    # Retry with more attempts since directory removal is difficult on Windows
                    self._retry_file_operation(shutil.rmtree, target_dir, max_retries=10)

                    # Extra delay after successful removal on Windows
                    if self._is_windows:
                        time.sleep(0.5)
                except KeyboardInterrupt as ke:
                    from fbuild.interrupt_utils import (
                        handle_keyboard_interrupt_properly,
                    )

                    handle_keyboard_interrupt_properly(ke)
                except Exception as e:
                    # If removal fails, we CANNOT use shutil.move() because it will nest directories
                    # We must use the fallback individual file operations instead
                    if self.show_progress:
                        print(f"  [Warning] Could not remove existing directory after 10 attempts: {e}")
                        print("  [Warning] Using individual file operations to overwrite...")
                    target_removal_failed = True
                    # DON'T re-raise - will use fallback path below

            # Use shutil.move() for entire directory tree - single atomic operation
            # But ONLY if target_removal_failed is False
            # (If target couldn't be removed, shutil.move() will nest directories incorrectly)
            if not target_removal_failed:
                try:
                    if single_subdir:
                        # Move single extracted subdirectory to target location
                        # shutil.move(src, dst) moves src TO dst (renames it)
                        if self.show_progress:
                            print(f"  [DEBUG] Moving {single_subdir.name} to {target_dir}")
                            print(f"  [DEBUG] Source: {single_subdir}")
                            print(f"  [DEBUG] Target: {target_dir}")
                            print(f"  [DEBUG] Source exists: {single_subdir.exists()}")
                            print(f"  [DEBUG] Target exists before: {target_dir.exists()}")

                        if self._is_windows:
                            result = self._retry_file_operation(shutil.move, str(single_subdir), str(target_dir))
                            if self.show_progress:
                                print(f"  [DEBUG] shutil.move returned: {result}")
                        else:
                            shutil.move(str(single_subdir), str(target_dir))

                        if self.show_progress:
                            print(f"  [DEBUG] Target exists after: {target_dir.exists()}")
                            if target_dir.exists() and target_dir.is_dir():
                                try:
                                    items = list(target_dir.iterdir())
                                    print(f"  [DEBUG] Target has {len(items)} items")
                                    if items:
                                        print(f"  [DEBUG] First 5 items: {[i.name for i in items[:5]]}")
                                except KeyboardInterrupt as ke:
                                    from fbuild.interrupt_utils import (
                                        handle_keyboard_interrupt_properly,
                                    )

                                    handle_keyboard_interrupt_properly(ke)
                                except Exception as e:
                                    print(f"  [DEBUG] Could not list target: {e}")
                    else:
                        # Multiple items - need to move temp_extract contents
                        # For this case, we need to move items individually (rare case)
                        raise Exception("Multiple items - need individual move")

                    if self.show_progress:
                        print(f"  Successfully moved to {target_dir.name}")

                except KeyboardInterrupt as ke:
                    from fbuild.interrupt_utils import (
                        handle_keyboard_interrupt_properly,
                    )

                    handle_keyboard_interrupt_properly(ke)
                except Exception as move_error:
                    # If shutil.move() fails, fall back to individual file operations
                    if self.show_progress:
                        print(f"  [Warning] Atomic move failed: {move_error}")
                        print("  Falling back to individual file operations...")

                    # Determine source directory for fallback
                    source_for_fallback = single_subdir if single_subdir else temp_extract

                    # Ensure target exists
                    target_dir.mkdir(parents=True, exist_ok=True)

                    # Get items with retry on Windows
                    def get_source_items():
                        return list(source_for_fallback.iterdir())

                    source_items = self._retry_file_operation(get_source_items) if self._is_windows else list(source_for_fallback.iterdir())

                    # Move items individually with retry
                    for item in source_items:
                        dest = target_dir / item.name
                        if dest.exists():
                            if dest.is_dir():
                                self._retry_file_operation(shutil.rmtree, dest, ignore_errors=True)
                            else:
                                self._retry_file_operation(dest.unlink)

                        # Try rename first, fall back to copy
                        try:
                            self._retry_file_operation(item.rename, dest)
                        except OSError:
                            if item.is_dir():
                                self._copytree_with_retry(item, dest)
                            else:
                                self._retry_file_operation(shutil.copy2, item, dest)

            else:
                # target_removal_failed is True - use individual file operations directly
                # Cannot use shutil.move() because target still exists and it would nest
                if self.show_progress:
                    print("  Using individual file copy to overwrite existing files...")

                # Determine source directory
                source_for_overwrite = single_subdir if single_subdir else temp_extract

                # Ensure target exists
                target_dir.mkdir(parents=True, exist_ok=True)

                # Get items with retry on Windows
                def get_source_items():
                    return list(source_for_overwrite.iterdir())

                source_items = self._retry_file_operation(get_source_items) if self._is_windows else list(source_for_overwrite.iterdir())
                assert source_items is not None

                # Copy/overwrite items individually with retry
                for item in source_items:
                    dest = target_dir / item.name
                    if dest.exists():
                        if dest.is_dir():
                            # Try to remove existing directory
                            try:
                                self._retry_file_operation(shutil.rmtree, dest, max_retries=10)
                            except KeyboardInterrupt as ke:
                                from fbuild.interrupt_utils import (
                                    handle_keyboard_interrupt_properly,
                                )

                                handle_keyboard_interrupt_properly(ke)
                            except Exception:
                                # If can't remove, skip this item (maybe locked)
                                if self.show_progress:
                                    print(f"  [Warning] Could not overwrite {dest.name}, skipping...")
                                continue
                        else:
                            try:
                                self._retry_file_operation(dest.unlink, max_retries=5)
                            except KeyboardInterrupt as ke:
                                from fbuild.interrupt_utils import (
                                    handle_keyboard_interrupt_properly,
                                )

                                handle_keyboard_interrupt_properly(ke)
                            except Exception:
                                # If can't remove file, skip
                                if self.show_progress:
                                    print(f"  [Warning] Could not overwrite {dest.name}, skipping...")
                                continue

                    # Try rename first, fall back to copy
                    try:
                        self._retry_file_operation(item.rename, dest)
                    except OSError:
                        if item.is_dir():
                            self._copytree_with_retry(item, dest)
                        else:
                            self._retry_file_operation(shutil.copy2, item, dest)

                if self.show_progress:
                    print(f"  Successfully extracted to {target_dir.name}")

        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

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
            # Extract .zip archive with progress tracking
            with zipfile.ZipFile(archive_path, "r") as zf:
                members = zf.namelist()
                total_members = len(members)

                if self.show_progress:
                    from tqdm import tqdm

                    with tqdm(total=total_members, unit="file", desc=f"Extracting {archive_path.name}") as pbar:
                        for member in members:
                            # On Windows, individual file extractions can hit Permission Errors
                            # due to file handle delays or antivirus/Windows Defender scanning
                            # Wrap each extract in retry logic
                            if self._is_windows:

                                def extract_member():
                                    zf.extract(member, temp_extract)

                                self._retry_file_operation(extract_member, max_retries=5)
                            else:
                                zf.extract(member, temp_extract)
                            pbar.update(1)
                else:
                    zf.extractall(temp_extract)

            # On Windows, force garbage collection and add LONG delay to let file handles close
            # Large archives (3000+ files) need extensive time for Windows to stabilize
            if self._is_windows:
                gc.collect()
                time.sleep(5.0)  # Increased to 5s - filesystem stabilization for large archives

            # Find the extracted directory
            # Wrap in retry logic - Windows may not have the directory handle ready yet
            def get_extracted_items():
                return list(temp_extract.iterdir())

            extracted_items = self._retry_file_operation(get_extracted_items) if self._is_windows else list(temp_extract.iterdir())
            assert extracted_items is not None

            # Check if single item is a directory - wrap in retry logic for Windows
            single_subdir = None
            if len(extracted_items) == 1:
                if self._is_windows:

                    def check_is_dir():
                        return extracted_items[0].is_dir()

                    is_single_dir = self._retry_file_operation(check_is_dir)
                else:
                    is_single_dir = extracted_items[0].is_dir()

                if is_single_dir:
                    # Single directory extracted - we'll move it atomically
                    single_subdir = extracted_items[0]

            # On Windows, add another delay before move operation
            if self._is_windows:
                time.sleep(1.0)  # Additional delay for directory handles

            # Move directory using shutil.move() for entire tree (atomic operation)
            # This is MUCH more reliable on Windows than iterating through individual files
            # Single atomic operation instead of 3000+ individual file operations
            if self.show_progress:
                print(f"Moving extracted files to {target_dir.name}...")

            # Track whether we need to remove target before move
            target_removal_failed = False

            # Remove existing target directory if it exists
            if target_dir.exists():
                if self.show_progress:
                    print(f"  Removing existing {target_dir.name}...")

                # On Windows, prepare for directory removal
                if self._is_windows:
                    gc.collect()  # Force garbage collection to release handles
                    time.sleep(1.0)  # Give Windows time to release handles

                try:
                    # Remove ignore_errors - let retry logic handle errors
                    # Retry with more attempts since directory removal is difficult on Windows
                    self._retry_file_operation(shutil.rmtree, target_dir, max_retries=10)

                    # Extra delay after successful removal on Windows
                    if self._is_windows:
                        time.sleep(0.5)
                except KeyboardInterrupt as ke:
                    from fbuild.interrupt_utils import (
                        handle_keyboard_interrupt_properly,
                    )

                    handle_keyboard_interrupt_properly(ke)
                except Exception as e:
                    # If removal fails, we CANNOT use shutil.move() because it will nest directories
                    # We must use the fallback individual file operations instead
                    if self.show_progress:
                        print(f"  [Warning] Could not remove existing directory after 10 attempts: {e}")
                        print("  [Warning] Using individual file operations to overwrite...")
                    target_removal_failed = True
                    # DON'T re-raise - will use fallback path below

            # Use shutil.move() for entire directory tree - single atomic operation
            # But ONLY if target_removal_failed is False
            # (If target couldn't be removed, shutil.move() will nest directories incorrectly)
            if not target_removal_failed:
                try:
                    if single_subdir:
                        # Move single extracted subdirectory to target location
                        # shutil.move(src, dst) moves src TO dst (renames it)
                        if self.show_progress:
                            print(f"  [DEBUG] Moving {single_subdir.name} to {target_dir}")
                            print(f"  [DEBUG] Source: {single_subdir}")
                            print(f"  [DEBUG] Target: {target_dir}")
                            print(f"  [DEBUG] Source exists: {single_subdir.exists()}")
                            print(f"  [DEBUG] Target exists before: {target_dir.exists()}")

                        if self._is_windows:
                            result = self._retry_file_operation(shutil.move, str(single_subdir), str(target_dir))
                            if self.show_progress:
                                print(f"  [DEBUG] shutil.move returned: {result}")
                        else:
                            shutil.move(str(single_subdir), str(target_dir))

                        if self.show_progress:
                            print(f"  [DEBUG] Target exists after: {target_dir.exists()}")
                            if target_dir.exists() and target_dir.is_dir():
                                try:
                                    items = list(target_dir.iterdir())
                                    print(f"  [DEBUG] Target has {len(items)} items")
                                    if items:
                                        print(f"  [DEBUG] First 5 items: {[i.name for i in items[:5]]}")
                                except KeyboardInterrupt as ke:
                                    from fbuild.interrupt_utils import (
                                        handle_keyboard_interrupt_properly,
                                    )

                                    handle_keyboard_interrupt_properly(ke)
                                except Exception as e:
                                    print(f"  [DEBUG] Could not list target: {e}")
                    else:
                        # Multiple items - need to move temp_extract contents
                        # For this case, we need to move items individually (rare case)
                        raise Exception("Multiple items - need individual move")

                    if self.show_progress:
                        print(f"  Successfully moved to {target_dir.name}")

                except KeyboardInterrupt as ke:
                    from fbuild.interrupt_utils import (
                        handle_keyboard_interrupt_properly,
                    )

                    handle_keyboard_interrupt_properly(ke)
                except Exception as move_error:
                    # If shutil.move() fails, fall back to individual file operations
                    if self.show_progress:
                        print(f"  [Warning] Atomic move failed: {move_error}")
                        print("  Falling back to individual file operations...")

                    # Determine source directory for fallback
                    source_for_fallback = single_subdir if single_subdir else temp_extract

                    # Ensure target exists
                    target_dir.mkdir(parents=True, exist_ok=True)

                    # Get items with retry on Windows
                    def get_source_items():
                        return list(source_for_fallback.iterdir())

                    source_items = self._retry_file_operation(get_source_items) if self._is_windows else list(source_for_fallback.iterdir())

                    # Move items individually with retry
                    for item in source_items:
                        dest = target_dir / item.name
                        if dest.exists():
                            if dest.is_dir():
                                self._retry_file_operation(shutil.rmtree, dest, ignore_errors=True)
                            else:
                                self._retry_file_operation(dest.unlink)

                        # Try rename first, fall back to copy
                        try:
                            self._retry_file_operation(item.rename, dest)
                        except OSError:
                            if item.is_dir():
                                self._copytree_with_retry(item, dest)
                            else:
                                self._retry_file_operation(shutil.copy2, item, dest)

            else:
                # target_removal_failed is True - use individual file operations directly
                # Cannot use shutil.move() because target still exists and it would nest
                if self.show_progress:
                    print("  Using individual file copy to overwrite existing files...")

                # Determine source directory
                source_for_overwrite = single_subdir if single_subdir else temp_extract

                # Ensure target exists
                target_dir.mkdir(parents=True, exist_ok=True)

                # Get items with retry on Windows
                def get_source_items():
                    return list(source_for_overwrite.iterdir())

                source_items = self._retry_file_operation(get_source_items) if self._is_windows else list(source_for_overwrite.iterdir())
                assert source_items is not None

                # Copy/overwrite items individually with retry
                for item in source_items:
                    dest = target_dir / item.name
                    if dest.exists():
                        if dest.is_dir():
                            # Try to remove existing directory
                            try:
                                self._retry_file_operation(shutil.rmtree, dest, max_retries=10)
                            except KeyboardInterrupt as ke:
                                from fbuild.interrupt_utils import (
                                    handle_keyboard_interrupt_properly,
                                )

                                handle_keyboard_interrupt_properly(ke)
                            except Exception:
                                # If can't remove, skip this item (maybe locked)
                                if self.show_progress:
                                    print(f"  [Warning] Could not overwrite {dest.name}, skipping...")
                                continue
                        else:
                            try:
                                self._retry_file_operation(dest.unlink, max_retries=5)
                            except KeyboardInterrupt as ke:
                                from fbuild.interrupt_utils import (
                                    handle_keyboard_interrupt_properly,
                                )

                                handle_keyboard_interrupt_properly(ke)
                            except Exception:
                                # If can't remove file, skip
                                if self.show_progress:
                                    print(f"  [Warning] Could not overwrite {dest.name}, skipping...")
                                continue

                    # Try rename first, fall back to copy
                    try:
                        self._retry_file_operation(item.rename, dest)
                    except OSError:
                        if item.is_dir():
                            self._copytree_with_retry(item, dest)
                        else:
                            self._retry_file_operation(shutil.copy2, item, dest)

                if self.show_progress:
                    print(f"  Successfully extracted to {target_dir.name}")

        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

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
            # Extract .tar.gz archive with progress tracking
            with tarfile.open(archive_path, "r:gz") as tar:
                members = tar.getmembers()
                total_members = len(members)

                if self.show_progress:
                    from tqdm import tqdm

                    with tqdm(total=total_members, unit="file", desc=f"Extracting {archive_path.name}") as pbar:
                        for member in members:
                            # On Windows, individual file extractions can hit Permission Errors
                            # due to file handle delays or antivirus/Windows Defender scanning
                            # Wrap each extract in retry logic
                            if self._is_windows:

                                def extract_member():
                                    tar.extract(member, temp_extract)

                                self._retry_file_operation(extract_member, max_retries=5)
                            else:
                                tar.extract(member, temp_extract)
                            pbar.update(1)
                else:
                    tar.extractall(temp_extract)

            # On Windows, force garbage collection and add LONG delay to let file handles close
            # Large archives (3000+ files) need extensive time for Windows to stabilize
            if self._is_windows:
                gc.collect()
                time.sleep(3.0)  # Increased to 3s - filesystem stabilization for large archives

            # Find the extracted directory
            # Wrap in retry logic - Windows may not have the directory handle ready yet
            def get_extracted_items():
                return list(temp_extract.iterdir())

            extracted_items = self._retry_file_operation(get_extracted_items) if self._is_windows else list(temp_extract.iterdir())
            assert extracted_items is not None

            # Check if single item is a directory - wrap in retry logic for Windows
            single_subdir = None
            if len(extracted_items) == 1:
                if self._is_windows:

                    def check_is_dir():
                        return extracted_items[0].is_dir()

                    is_single_dir = self._retry_file_operation(check_is_dir)
                else:
                    is_single_dir = extracted_items[0].is_dir()

                if is_single_dir:
                    # Single directory extracted - we'll move it atomically
                    single_subdir = extracted_items[0]

            # On Windows, add another delay before move operation
            if self._is_windows:
                time.sleep(1.0)  # Additional delay for directory handles

            # Move directory using shutil.move() for entire tree (atomic operation)
            # This is MUCH more reliable on Windows than iterating through individual files
            # Single atomic operation instead of 3000+ individual file operations
            if self.show_progress:
                print(f"Moving extracted files to {target_dir.name}...")

            # Track whether we need to remove target before move
            target_removal_failed = False

            # Remove existing target directory if it exists
            if target_dir.exists():
                if self.show_progress:
                    print(f"  Removing existing {target_dir.name}...")

                # On Windows, prepare for directory removal
                if self._is_windows:
                    gc.collect()  # Force garbage collection to release handles
                    time.sleep(1.0)  # Give Windows time to release handles

                try:
                    # Remove ignore_errors - let retry logic handle errors
                    # Retry with more attempts since directory removal is difficult on Windows
                    self._retry_file_operation(shutil.rmtree, target_dir, max_retries=10)

                    # Extra delay after successful removal on Windows
                    if self._is_windows:
                        time.sleep(0.5)
                except KeyboardInterrupt as ke:
                    from fbuild.interrupt_utils import (
                        handle_keyboard_interrupt_properly,
                    )

                    handle_keyboard_interrupt_properly(ke)
                except Exception as e:
                    # If removal fails, we CANNOT use shutil.move() because it will nest directories
                    # We must use the fallback individual file operations instead
                    if self.show_progress:
                        print(f"  [Warning] Could not remove existing directory after 10 attempts: {e}")
                        print("  [Warning] Using individual file operations to overwrite...")
                    target_removal_failed = True
                    # DON'T re-raise - will use fallback path below

            # Use shutil.move() for entire directory tree - single atomic operation
            # But ONLY if target_removal_failed is False
            # (If target couldn't be removed, shutil.move() will nest directories incorrectly)
            if not target_removal_failed:
                try:
                    if single_subdir:
                        # Move single extracted subdirectory to target location
                        # shutil.move(src, dst) moves src TO dst (renames it)
                        if self.show_progress:
                            print(f"  [DEBUG] Moving {single_subdir.name} to {target_dir}")
                            print(f"  [DEBUG] Source: {single_subdir}")
                            print(f"  [DEBUG] Target: {target_dir}")
                            print(f"  [DEBUG] Source exists: {single_subdir.exists()}")
                            print(f"  [DEBUG] Target exists before: {target_dir.exists()}")

                        if self._is_windows:
                            result = self._retry_file_operation(shutil.move, str(single_subdir), str(target_dir))
                            if self.show_progress:
                                print(f"  [DEBUG] shutil.move returned: {result}")
                        else:
                            shutil.move(str(single_subdir), str(target_dir))

                        if self.show_progress:
                            print(f"  [DEBUG] Target exists after: {target_dir.exists()}")
                            if target_dir.exists() and target_dir.is_dir():
                                try:
                                    items = list(target_dir.iterdir())
                                    print(f"  [DEBUG] Target has {len(items)} items")
                                    if items:
                                        print(f"  [DEBUG] First 5 items: {[i.name for i in items[:5]]}")
                                except KeyboardInterrupt as ke:
                                    from fbuild.interrupt_utils import (
                                        handle_keyboard_interrupt_properly,
                                    )

                                    handle_keyboard_interrupt_properly(ke)
                                except Exception as e:
                                    print(f"  [DEBUG] Could not list target: {e}")
                    else:
                        # Multiple items - need to move temp_extract contents
                        # For this case, we need to move items individually (rare case)
                        raise Exception("Multiple items - need individual move")

                    if self.show_progress:
                        print(f"  Successfully moved to {target_dir.name}")

                except KeyboardInterrupt as ke:
                    from fbuild.interrupt_utils import (
                        handle_keyboard_interrupt_properly,
                    )

                    handle_keyboard_interrupt_properly(ke)
                except Exception as move_error:
                    # If shutil.move() fails, fall back to individual file operations
                    if self.show_progress:
                        print(f"  [Warning] Atomic move failed: {move_error}")
                        print("  Falling back to individual file operations...")

                    # Determine source directory for fallback
                    source_for_fallback = single_subdir if single_subdir else temp_extract

                    # Ensure target exists
                    target_dir.mkdir(parents=True, exist_ok=True)

                    # Get items with retry on Windows
                    def get_source_items():
                        return list(source_for_fallback.iterdir())

                    source_items = self._retry_file_operation(get_source_items) if self._is_windows else list(source_for_fallback.iterdir())

                    # Move items individually with retry
                    for item in source_items:
                        dest = target_dir / item.name
                        if dest.exists():
                            if dest.is_dir():
                                self._retry_file_operation(shutil.rmtree, dest, ignore_errors=True)
                            else:
                                self._retry_file_operation(dest.unlink)

                        # Try rename first, fall back to copy
                        try:
                            self._retry_file_operation(item.rename, dest)
                        except OSError:
                            if item.is_dir():
                                self._copytree_with_retry(item, dest)
                            else:
                                self._retry_file_operation(shutil.copy2, item, dest)

            else:
                # target_removal_failed is True - use individual file operations directly
                # Cannot use shutil.move() because target still exists and it would nest
                if self.show_progress:
                    print("  Using individual file copy to overwrite existing files...")

                # Determine source directory
                source_for_overwrite = single_subdir if single_subdir else temp_extract

                # Ensure target exists
                target_dir.mkdir(parents=True, exist_ok=True)

                # Get items with retry on Windows
                def get_source_items():
                    return list(source_for_overwrite.iterdir())

                source_items = self._retry_file_operation(get_source_items) if self._is_windows else list(source_for_overwrite.iterdir())
                assert source_items is not None

                # Copy/overwrite items individually with retry
                for item in source_items:
                    dest = target_dir / item.name
                    if dest.exists():
                        if dest.is_dir():
                            # Try to remove existing directory
                            try:
                                self._retry_file_operation(shutil.rmtree, dest, max_retries=10)
                            except KeyboardInterrupt as ke:
                                from fbuild.interrupt_utils import (
                                    handle_keyboard_interrupt_properly,
                                )

                                handle_keyboard_interrupt_properly(ke)
                            except Exception:
                                # If can't remove, skip this item (maybe locked)
                                if self.show_progress:
                                    print(f"  [Warning] Could not overwrite {dest.name}, skipping...")
                                continue
                        else:
                            try:
                                self._retry_file_operation(dest.unlink, max_retries=5)
                            except KeyboardInterrupt as ke:
                                from fbuild.interrupt_utils import (
                                    handle_keyboard_interrupt_properly,
                                )

                                handle_keyboard_interrupt_properly(ke)
                            except Exception:
                                # If can't remove file, skip
                                if self.show_progress:
                                    print(f"  [Warning] Could not overwrite {dest.name}, skipping...")
                                continue

                    # Try rename first, fall back to copy
                    try:
                        self._retry_file_operation(item.rename, dest)
                    except OSError:
                        if item.is_dir():
                            self._copytree_with_retry(item, dest)
                        else:
                            self._retry_file_operation(shutil.copy2, item, dest)

                if self.show_progress:
                    print(f"  Successfully extracted to {target_dir.name}")

        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

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
