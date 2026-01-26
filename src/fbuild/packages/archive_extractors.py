"""Archive Format-Specific Extractors.

This module provides format-specific extractors for tar.xz, tar.gz, and zip archives,
using a strategy pattern to eliminate code duplication.
"""

import gc
import tarfile
import time
import zipfile
from abc import ABC, abstractmethod
from pathlib import Path
from typing import Protocol, runtime_checkable

from .archive_strategies import DirectoryMover, FileOperations
from .downloader import ExtractionError


@runtime_checkable
class IArchiveExtractor(Protocol):
    """Protocol for archive extractors."""

    def extract(self, archive_path: Path, target_dir: Path) -> None:
        """Extract archive to target directory.

        Args:
            archive_path: Path to the archive file
            target_dir: Directory to extract contents into

        Raises:
            ExtractionError: If extraction fails
        """
        ...


class BaseArchiveExtractor(ABC):
    """Base class for archive extractors using template method pattern.

    Defines the extraction workflow with hooks for format-specific operations.
    """

    def __init__(self, file_ops: FileOperations, directory_mover: DirectoryMover, show_progress: bool = False, is_windows: bool = False):
        """Initialize base extractor.

        Args:
            file_ops: File operations helper
            directory_mover: Directory mover helper
            show_progress: Whether to show progress messages
            is_windows: Whether running on Windows
        """
        self.file_ops = file_ops
        self.directory_mover = directory_mover
        self.show_progress = show_progress
        self.is_windows = is_windows

    def extract(self, archive_path: Path, target_dir: Path) -> None:
        """Extract archive to target directory (template method).

        Args:
            archive_path: Path to the archive file
            target_dir: Directory to extract contents into

        Raises:
            ExtractionError: If extraction fails
        """
        # Create temp extraction directory
        temp_extract = target_dir.parent / f"temp_extract_{archive_path.name}"
        temp_extract.mkdir(parents=True, exist_ok=True)

        try:
            # Step 1: Extract archive to temp directory
            self._extract_to_temp(archive_path, temp_extract)

            # Step 2: Post-extraction delay on Windows for file handle stabilization
            if self.is_windows:
                gc.collect()
                time.sleep(self._get_windows_stabilization_delay())

            # Step 3: Find extracted directory structure
            extracted_items = self.file_ops.list_directory(temp_extract, max_retries=10 if self.is_windows else 5)

            # Determine source directory (single subdir or temp_extract)
            source_dir = self._determine_source_directory(extracted_items, temp_extract)

            # Step 4: Windows pre-move delay
            if self.is_windows:
                time.sleep(1.0)

            # Step 5: Move extracted content to target
            self.directory_mover.move_directory(source_dir, target_dir, is_windows=self.is_windows)

        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
        except Exception as e:
            raise ExtractionError(f"Failed to extract archive: {e}")
        finally:
            # Clean up temp directory
            if temp_extract.exists():
                import shutil

                shutil.rmtree(temp_extract, ignore_errors=True)

    @abstractmethod
    def _extract_to_temp(self, archive_path: Path, temp_extract: Path) -> None:
        """Extract archive contents to temporary directory.

        Args:
            archive_path: Path to the archive file
            temp_extract: Temporary extraction directory

        Raises:
            ExtractionError: If extraction fails
        """
        ...

    @abstractmethod
    def _get_windows_stabilization_delay(self) -> float:
        """Get Windows filesystem stabilization delay in seconds.

        Returns:
            Delay in seconds
        """
        ...

    def _determine_source_directory(self, extracted_items: list[Path], temp_extract: Path) -> Path:
        """Determine source directory for move operation.

        If archive extracts to a single subdirectory, return that.
        Otherwise, return temp_extract.

        Args:
            extracted_items: List of extracted items
            temp_extract: Temporary extraction directory

        Returns:
            Source directory path
        """
        if len(extracted_items) == 1 and self.file_ops.is_directory(extracted_items[0]):
            return extracted_items[0]
        return temp_extract


class TarXzExtractor(BaseArchiveExtractor):
    """Extractor for .tar.xz archives."""

    def _extract_to_temp(self, archive_path: Path, temp_extract: Path) -> None:
        """Extract .tar.xz archive to temporary directory.

        Args:
            archive_path: Path to the .tar.xz archive file
            temp_extract: Temporary extraction directory
        """
        with tarfile.open(archive_path, "r:xz") as tar:
            members = tar.getmembers()
            total_members = len(members)

            if self.show_progress:
                from tqdm import tqdm

                with tqdm(total=total_members, unit="file", desc=f"Extracting {archive_path.name}") as pbar:
                    for member in members:
                        if self.is_windows:
                            # Wrap each extract in retry logic for Windows
                            def extract_member():
                                tar.extract(member, temp_extract)

                            self.file_ops.retry_strategy.retry_operation(extract_member, max_retries=5, show_progress=self.show_progress)
                        else:
                            tar.extract(member, temp_extract)
                        pbar.update(1)
            else:
                tar.extractall(temp_extract)

    def _get_windows_stabilization_delay(self) -> float:
        """Get Windows filesystem stabilization delay for tar.xz.

        Returns:
            5.0 seconds (large archives need extensive stabilization time)
        """
        return 5.0


class TarGzExtractor(BaseArchiveExtractor):
    """Extractor for .tar.gz archives."""

    def _extract_to_temp(self, archive_path: Path, temp_extract: Path) -> None:
        """Extract .tar.gz archive to temporary directory.

        Args:
            archive_path: Path to the .tar.gz archive file
            temp_extract: Temporary extraction directory
        """
        with tarfile.open(archive_path, "r:gz") as tar:
            members = tar.getmembers()
            total_members = len(members)

            if self.show_progress:
                from tqdm import tqdm

                with tqdm(total=total_members, unit="file", desc=f"Extracting {archive_path.name}") as pbar:
                    for member in members:
                        if self.is_windows:
                            # Wrap each extract in retry logic for Windows
                            def extract_member():
                                tar.extract(member, temp_extract)

                            self.file_ops.retry_strategy.retry_operation(extract_member, max_retries=5, show_progress=self.show_progress)
                        else:
                            tar.extract(member, temp_extract)
                        pbar.update(1)
            else:
                tar.extractall(temp_extract)

    def _get_windows_stabilization_delay(self) -> float:
        """Get Windows filesystem stabilization delay for tar.gz.

        Returns:
            3.0 seconds (moderate stabilization time)
        """
        return 3.0


class ZipExtractor(BaseArchiveExtractor):
    """Extractor for .zip archives."""

    def _extract_to_temp(self, archive_path: Path, temp_extract: Path) -> None:
        """Extract .zip archive to temporary directory.

        Args:
            archive_path: Path to the .zip archive file
            temp_extract: Temporary extraction directory
        """
        with zipfile.ZipFile(archive_path, "r") as zf:
            members = zf.namelist()
            total_members = len(members)

            if self.show_progress:
                from tqdm import tqdm

                with tqdm(total=total_members, unit="file", desc=f"Extracting {archive_path.name}") as pbar:
                    for member in members:
                        if self.is_windows:
                            # Wrap each extract in retry logic for Windows
                            def extract_member():
                                zf.extract(member, temp_extract)

                            self.file_ops.retry_strategy.retry_operation(extract_member, max_retries=5, show_progress=self.show_progress)
                        else:
                            zf.extract(member, temp_extract)
                        pbar.update(1)
            else:
                zf.extractall(temp_extract)

    def _get_windows_stabilization_delay(self) -> float:
        """Get Windows filesystem stabilization delay for zip.

        Returns:
            5.0 seconds (large archives need extensive stabilization time)
        """
        return 5.0


class ArchiveExtractorFactory:
    """Factory for creating format-specific archive extractors."""

    @staticmethod
    def create_extractor(
        archive_path: Path,
        file_ops: FileOperations,
        directory_mover: DirectoryMover,
        show_progress: bool = False,
        is_windows: bool = False,
    ) -> IArchiveExtractor:
        """Create appropriate extractor based on archive file extension.

        Args:
            archive_path: Path to the archive file
            file_ops: File operations helper
            directory_mover: Directory mover helper
            show_progress: Whether to show progress messages
            is_windows: Whether running on Windows

        Returns:
            Archive extractor instance

        Raises:
            ValueError: If archive format is not supported
        """
        archive_name = archive_path.name.lower()

        # Detect archive type by extension
        if archive_name.endswith(".zip"):
            return ZipExtractor(file_ops, directory_mover, show_progress, is_windows)
        elif archive_name.endswith(".tar.xz") or archive_name.endswith(".txz"):
            return TarXzExtractor(file_ops, directory_mover, show_progress, is_windows)
        elif archive_name.endswith(".tar.gz") or archive_name.endswith(".tgz"):
            return TarGzExtractor(file_ops, directory_mover, show_progress, is_windows)
        else:
            # Default to tar.xz for backwards compatibility
            return TarXzExtractor(file_ops, directory_mover, show_progress, is_windows)
