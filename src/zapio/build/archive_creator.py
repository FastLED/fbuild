"""Archive Creator.

This module handles creating static library archives (.a files) from compiled
object files using the archiver tool (ar).

Design:
    - Wraps ar command execution
    - Creates .a archives from object files
    - Provides clear error messages
    - Shows archive size information
"""

import subprocess
from pathlib import Path
from typing import List


class ArchiveError(Exception):
    """Raised when archive creation operations fail."""
    pass


class ArchiveCreator:
    """Creates static library archives from object files.

    This class handles:
    - Running archiver (ar) commands
    - Creating .a archives from object files
    - Validating archive creation
    - Showing size information
    """

    def __init__(self, show_progress: bool = True):
        """Initialize archive creator.

        Args:
            show_progress: Whether to show archive creation progress
        """
        self.show_progress = show_progress

    def create_archive(
        self,
        ar_path: Path,
        archive_path: Path,
        object_files: List[Path]
    ) -> Path:
        """Create static library archive from object files.

        Args:
            ar_path: Path to archiver tool (ar)
            archive_path: Path for output .a file
            object_files: List of object file paths to archive

        Returns:
            Path to generated archive file

        Raises:
            ArchiveError: If archive creation fails
        """
        if not object_files:
            raise ArchiveError("No object files provided for archive")

        if not ar_path.exists():
            raise ArchiveError(
                f"Archiver not found: {ar_path}. Ensure toolchain is installed."
            )

        # Ensure archive directory exists
        archive_path.parent.mkdir(parents=True, exist_ok=True)

        # Build archiver command
        # 'rcs' flags: r=insert/replace, c=create, s=index (ranlib)
        cmd = [str(ar_path), "rcs", str(archive_path)]
        cmd.extend([str(obj) for obj in object_files])

        # Execute archiver
        if self.show_progress:
            print(f"Creating {archive_path.name} archive from {len(object_files)} object files...")

        try:
            result = subprocess.run(
                cmd,
                capture_output=True,
                text=True,
                timeout=60
            )

            if result.returncode != 0:
                error_msg = f"Archive creation failed for {archive_path.name}\n"
                error_msg += f"stderr: {result.stderr}\n"
                error_msg += f"stdout: {result.stdout}"
                raise ArchiveError(error_msg)

            if not archive_path.exists():
                raise ArchiveError(f"Archive was not created: {archive_path}")

            if self.show_progress:
                size = archive_path.stat().st_size
                print(f"✓ Created {archive_path.name}: {size:,} bytes ({size / 1024 / 1024:.2f} MB)")

            return archive_path

        except subprocess.TimeoutExpired as e:
            raise ArchiveError(f"Archive creation timeout for {archive_path.name}") from e
        except KeyboardInterrupt as ke:
            from zapio.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            if isinstance(e, ArchiveError):
                raise
            raise ArchiveError(f"Failed to create archive {archive_path.name}: {e}") from e

    def create_core_archive(
        self,
        ar_path: Path,
        build_dir: Path,
        object_files: List[Path]
    ) -> Path:
        """Create core.a archive from core object files.

        Convenience method for creating the standard core.a archive.

        Args:
            ar_path: Path to archiver tool (ar)
            build_dir: Build directory
            object_files: List of core object file paths

        Returns:
            Path to generated core.a file

        Raises:
            ArchiveError: If archive creation fails
        """
        archive_path = build_dir / "core.a"
        return self.create_archive(ar_path, archive_path, object_files)
