"""Build utilities for Zapio.

This module provides utility functions for build operations like
printing size information and formatting build output.
"""

import os
import stat
import shutil
from pathlib import Path
from typing import Any, Callable, Optional

from ..build.linker import SizeInfo


class SizeInfoPrinter:
    """Utility class for printing firmware size information."""

    @staticmethod
    def print_size_info(size_info: Optional[SizeInfo]) -> None:
        """
        Print firmware size information in a formatted display.

        Args:
            size_info: Size information from linker (None to skip printing)
        """
        if not size_info:
            return

        print("Firmware Size:")
        print(f"  Program:  {size_info.total_flash:6d} bytes", end="")
        if size_info.flash_percent is not None:
            print(
                f" ({size_info.flash_percent:5.1f}% of {size_info.max_flash} bytes)"
            )
        else:
            print()

        print(f"  Data:     {size_info.data:6d} bytes")
        print(f"  BSS:      {size_info.bss:6d} bytes")
        print(f"  RAM:      {size_info.total_ram:6d} bytes", end="")
        if size_info.ram_percent is not None:
            print(f" ({size_info.ram_percent:5.1f}% of {size_info.max_ram} bytes)")
        else:
            print()


def remove_readonly(func: Callable[[str], None], path: str, excinfo: Any) -> None:
    """
    Error handler for shutil.rmtree on Windows.

    On Windows, read-only files cannot be deleted and will cause
    shutil.rmtree to fail. This handler removes the read-only attribute
    and retries the operation.

    Args:
        func: The function that raised the exception
        path: The path to the file/directory
        excinfo: Exception information (unused)
    """
    os.chmod(path, stat.S_IWRITE)
    func(path)


def safe_rmtree(path: Path, max_retries: int = 3) -> None:
    """
    Safely remove a directory tree, handling Windows-specific issues.

    This function handles common Windows issues with directory deletion:
    - Read-only files
    - Locked files (with retries)
    - Hidden system files

    Args:
        path: Path to directory to remove
        max_retries: Maximum number of retry attempts for locked files

    Raises:
        OSError: If directory cannot be removed after all retries
    """
    import time

    if not path.exists():
        return

    for attempt in range(max_retries):
        try:
            shutil.rmtree(path, onerror=remove_readonly)
            return
        except OSError as e:
            if attempt < max_retries - 1:
                # Wait a bit and retry (files might be temporarily locked)
                time.sleep(0.5)
            else:
                # Last attempt failed, raise the error
                raise OSError(
                    f"Failed to remove directory {path} after {max_retries} attempts: {e}"
                ) from e
