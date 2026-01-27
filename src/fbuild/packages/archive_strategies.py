"""Archive Extraction Retry Strategies.

This module provides retry strategies and file operations for reliable archive extraction,
particularly on Windows where file handle delays can cause PermissionError and OSError.
"""

import gc
import logging
import shutil
import time
from pathlib import Path
from typing import Any, Callable, Protocol, runtime_checkable


@runtime_checkable
class IRetryStrategy(Protocol):
    """Protocol for retry strategies."""

    def retry_operation(
        self,
        operation: Callable[..., Any],
        *args: Any,
        max_retries: int = 5,
        show_progress: bool = False,
        **kwargs: Any,
    ) -> Any:
        """Retry an operation with platform-specific strategy.

        Args:
            operation: Function to call
            *args: Positional arguments for the operation
            max_retries: Maximum number of retry attempts
            show_progress: Whether to show retry progress messages
            **kwargs: Keyword arguments for the operation

        Returns:
            Result of the operation

        Raises:
            The last exception if all retries fail
        """
        ...


class WindowsRetryStrategy:
    """Retry strategy for Windows with exponential backoff.

    On Windows, file operations can fail with PermissionError, OSError,
    or FileNotFoundError due to file handle delays. This strategy retries
    with exponential backoff and forced garbage collection.
    """

    def retry_operation(
        self,
        operation: Callable[..., Any],
        *args: Any,
        max_retries: int = 5,
        show_progress: bool = False,
        **kwargs: Any,
    ) -> Any:
        """Retry an operation with exponential backoff on Windows.

        Args:
            operation: Function to call (e.g., Path.unlink, shutil.rmtree)
            *args: Positional arguments for the operation
            max_retries: Maximum number of retry attempts
            show_progress: Whether to show retry progress messages
            **kwargs: Keyword arguments for the operation

        Returns:
            Result of the operation

        Raises:
            The last exception if all retries fail
        """
        delay = 0.05  # Start with 50ms
        last_error = None

        for attempt in range(max_retries):
            try:
                if attempt > 0:
                    gc.collect()  # Force garbage collection
                    time.sleep(delay)
                    if show_progress:
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


class UnixRetryStrategy:
    """Retry strategy for Unix-like systems (no retry needed).

    On Unix-like systems, file operations typically don't suffer from
    the same handle delay issues as Windows, so no retry is needed.
    """

    def retry_operation(
        self,
        operation: Callable[..., Any],
        *args: Any,
        max_retries: int = 5,
        show_progress: bool = False,
        **kwargs: Any,
    ) -> Any:
        """Execute operation directly without retry (Unix has no handle delays).

        Args:
            operation: Function to call
            *args: Positional arguments for the operation
            max_retries: Ignored (no retries needed on Unix)
            show_progress: Ignored (no progress to show)
            **kwargs: Keyword arguments for the operation

        Returns:
            Result of the operation
        """
        return operation(*args, **kwargs)


class FileOperations:
    """File operations with retry logic.

    Provides common file operations (list, move, copy, remove) with
    platform-appropriate retry strategies.
    """

    def __init__(self, retry_strategy: IRetryStrategy, show_progress: bool = False):
        """Initialize file operations.

        Args:
            retry_strategy: Retry strategy to use
            show_progress: Whether to show progress messages
        """
        self.retry_strategy = retry_strategy
        self.show_progress = show_progress

    def list_directory(self, path: Path, max_retries: int = 5) -> list[Path]:
        """List directory contents with retry.

        Args:
            path: Directory to list
            max_retries: Maximum retry attempts

        Returns:
            List of paths in the directory
        """

        def get_items():
            return list(path.iterdir())

        result = self.retry_strategy.retry_operation(get_items, max_retries=max_retries, show_progress=self.show_progress)
        assert result is not None
        return result

    def is_directory(self, path: Path, max_retries: int = 5) -> bool:
        """Check if path is a directory with retry.

        Args:
            path: Path to check
            max_retries: Maximum retry attempts

        Returns:
            True if path is a directory
        """

        def check_is_dir():
            return path.is_dir()

        return self.retry_strategy.retry_operation(check_is_dir, max_retries=max_retries, show_progress=self.show_progress)

    def remove_file(self, path: Path, max_retries: int = 5) -> None:
        """Remove a file with retry.

        Args:
            path: File to remove
            max_retries: Maximum retry attempts
        """
        self.retry_strategy.retry_operation(path.unlink, max_retries=max_retries, show_progress=self.show_progress)

    def remove_tree(self, path: Path, max_retries: int = 10, ignore_errors: bool = False) -> None:
        """Remove a directory tree with retry.

        Args:
            path: Directory to remove
            max_retries: Maximum retry attempts
            ignore_errors: Whether to ignore errors
        """
        self.retry_strategy.retry_operation(shutil.rmtree, path, ignore_errors=ignore_errors, max_retries=max_retries, show_progress=self.show_progress)

    def copy_file(self, src: Path, dst: Path, max_retries: int = 5) -> None:
        """Copy a file with retry.

        Args:
            src: Source file
            dst: Destination file
            max_retries: Maximum retry attempts
        """
        self.retry_strategy.retry_operation(shutil.copy2, src, dst, max_retries=max_retries, show_progress=self.show_progress)

    def rename(self, src: Path, dst: Path, max_retries: int = 5) -> None:
        """Rename/move a file or directory with retry.

        Args:
            src: Source path
            dst: Destination path
            max_retries: Maximum retry attempts
        """
        self.retry_strategy.retry_operation(src.rename, dst, max_retries=max_retries, show_progress=self.show_progress)

    def move(self, src: Path, dst: Path, max_retries: int = 5) -> Any:
        """Move a file or directory with retry.

        Args:
            src: Source path
            dst: Destination path
            max_retries: Maximum retry attempts

        Returns:
            Result of shutil.move()
        """
        return self.retry_strategy.retry_operation(shutil.move, str(src), str(dst), max_retries=max_retries, show_progress=self.show_progress)

    def create_directory(self, path: Path, parents: bool = True, exist_ok: bool = True, max_retries: int = 5) -> None:
        """Create a directory with retry.

        Args:
            path: Directory to create
            parents: Create parent directories
            exist_ok: Don't raise error if directory exists
            max_retries: Maximum retry attempts
        """
        self.retry_strategy.retry_operation(path.mkdir, parents=parents, exist_ok=exist_ok, max_retries=max_retries, show_progress=self.show_progress)

    def copytree_with_retry(self, src: Path, dst: Path) -> None:
        """Recursively copy directory tree with retry logic for each operation.

        Unlike shutil.copytree, this retries each individual file/directory operation,
        which is more robust on Windows where file handles may not be immediately available.

        Args:
            src: Source directory path
            dst: Destination directory path
        """
        # Create destination directory with retry
        self.create_directory(dst)

        # Get items with retry
        items = self.list_directory(src)

        for item in items:
            src_item = item
            dst_item = dst / item.name

            # Check if item is a directory with retry
            if self.is_directory(src_item):
                # Recursively copy subdirectory
                self.copytree_with_retry(src_item, dst_item)
            else:
                # Copy file with retry
                self.copy_file(src_item, dst_item)


class DirectoryMover:
    """Handles directory moving with atomic and fallback strategies.

    Provides reliable directory moving with support for:
    - Atomic move using shutil.move()
    - Fallback to individual file operations if atomic move fails
    - Proper target removal before move
    """

    def __init__(self, file_ops: FileOperations, show_progress: bool = False):
        """Initialize directory mover.

        Args:
            file_ops: File operations helper
            show_progress: Whether to show progress messages
        """
        self.file_ops = file_ops
        self.show_progress = show_progress

    def move_directory(self, source: Path, target: Path, is_windows: bool = False) -> None:
        """Move directory from source to target with atomic and fallback strategies.

        Args:
            source: Source directory path
            target: Target directory path
            is_windows: Whether running on Windows (enables delays)
        """
        if self.show_progress:
            print(f"Moving extracted files to {target.name}...")

        # Track whether target removal failed
        target_removal_failed = False

        # Remove existing target directory if it exists
        if target.exists():
            if self.show_progress:
                print(f"  Removing existing {target.name}...")

            # On Windows, prepare for directory removal
            if is_windows:
                gc.collect()  # Force garbage collection to release handles
                time.sleep(1.0)  # Give Windows time to release handles

            try:
                # Retry with more attempts since directory removal is difficult on Windows
                self.file_ops.remove_tree(target, max_retries=10)

                # Extra delay after successful removal on Windows
                if is_windows:
                    time.sleep(0.5)
            except KeyboardInterrupt as ke:
                from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

                handle_keyboard_interrupt_properly(ke)
            except Exception as e:
                # If removal fails, we CANNOT use shutil.move() because it will nest directories
                # We must use the fallback individual file operations instead
                if self.show_progress:
                    print(f"  [Warning] Could not remove existing directory after 10 attempts: {e}")
                    print("  [Warning] Using individual file operations to overwrite...")
                target_removal_failed = True
                # DON'T re-raise - will use fallback path below

        # Try atomic move first (if target was successfully removed or didn't exist)
        if not target_removal_failed:
            try:
                self._atomic_move(source, target, is_windows)
                if self.show_progress:
                    print(f"  Successfully moved to {target.name}")
                return
            except KeyboardInterrupt as ke:
                from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

                handle_keyboard_interrupt_properly(ke)
            except Exception as move_error:
                # If shutil.move() fails, fall back to individual file operations
                if self.show_progress:
                    print(f"  [Warning] Atomic move failed: {move_error}")
                    print("  Falling back to individual file operations...")

        # Fallback: individual file operations
        self._fallback_move(source, target)
        if self.show_progress:
            print(f"  Successfully extracted to {target.name}")

    def _atomic_move(self, source: Path, target: Path, is_windows: bool) -> None:
        """Attempt atomic directory move using shutil.move().

        Args:
            source: Source directory path
            target: Target directory path
            is_windows: Whether running on Windows

        Raises:
            Exception: If atomic move fails
        """
        if self.show_progress:
            logging.debug(f"Moving {source.name} to {target}")
            logging.debug(f"Source: {source}")
            logging.debug(f"Target: {target}")
            logging.debug(f"Source exists: {source.exists()}")
            logging.debug(f"Target exists before: {target.exists()}")

        if is_windows:
            result = self.file_ops.move(source, target)
            if self.show_progress:
                logging.debug(f"shutil.move returned: {result}")
        else:
            shutil.move(str(source), str(target))

        if self.show_progress:
            logging.debug(f"Target exists after: {target.exists()}")
            if target.exists() and target.is_dir():
                try:
                    items = list(target.iterdir())
                    logging.debug(f"Target has {len(items)} items")
                    if items:
                        logging.debug(f"First 5 items: {[i.name for i in items[:5]]}")
                except KeyboardInterrupt as ke:
                    from fbuild.interrupt_utils import (
                        handle_keyboard_interrupt_properly,
                    )

                    handle_keyboard_interrupt_properly(ke)
                except Exception as e:
                    logging.debug(f"Could not list target: {e}")

    def _fallback_move(self, source: Path, target: Path) -> None:
        """Move directory using individual file operations (fallback strategy).

        Args:
            source: Source directory path
            target: Target directory path
        """
        if self.show_progress:
            print("  Using individual file operations...")

        # Ensure target exists
        self.file_ops.create_directory(target)

        # Get items with retry
        source_items = self.file_ops.list_directory(source)

        # Move/copy items individually with retry
        for item in source_items:
            dest = target / item.name

            # Remove existing destination if present
            if dest.exists():
                try:
                    if dest.is_dir():
                        self.file_ops.remove_tree(dest, max_retries=10)
                    else:
                        self.file_ops.remove_file(dest, max_retries=5)
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

            # Try rename first, fall back to copy
            try:
                self.file_ops.rename(item, dest)
            except OSError:
                if item.is_dir():
                    self.file_ops.copytree_with_retry(item, dest)
                else:
                    self.file_ops.copy_file(item, dest)
