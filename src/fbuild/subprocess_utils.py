"""Subprocess utilities for platform-safe process execution.

This module provides wrappers around subprocess module that automatically
apply platform-specific flags to prevent console window flashing on Windows.
"""

import subprocess
import sys
from typing import Any


def get_subprocess_creation_flags() -> int:
    """Get platform-specific subprocess creation flags.

    Returns:
        - Windows: subprocess.CREATE_NO_WINDOW (prevents console window)
        - Other platforms: 0 (no special flags)
    """
    if sys.platform == "win32":
        return subprocess.CREATE_NO_WINDOW
    return 0


def safe_run(cmd: list[str], **kwargs: Any) -> subprocess.CompletedProcess:
    """Execute subprocess.run with platform-specific flags.

    Automatically applies:
    - CREATE_NO_WINDOW on Windows (prevents console window)
    - stdin=DEVNULL (prevents console input handle inheritance)

    The stdin redirect prevents keyboard input issues on Windows where
    child processes can steal keystrokes from the parent terminal by
    inheriting the console input buffer handle.

    Args:
        cmd: Command and arguments (same as subprocess.run)
        **kwargs: Additional arguments passed to subprocess.run

    Returns:
        CompletedProcess result from subprocess.run

    Note:
        - If 'creationflags' is explicitly provided in kwargs,
          it will be OR'd with platform defaults to preserve custom flags.
        - If 'stdin' is explicitly provided in kwargs, it will be used as-is.
          Otherwise, stdin is automatically redirected to subprocess.DEVNULL.
    """
    default_flags = get_subprocess_creation_flags()

    if "creationflags" in kwargs:
        kwargs["creationflags"] = kwargs["creationflags"] | default_flags
    elif default_flags:
        kwargs["creationflags"] = default_flags

    # Auto-redirect stdin to prevent console input handle inheritance
    # This prevents child processes from stealing keystrokes on Windows
    if "stdin" not in kwargs:
        kwargs["stdin"] = subprocess.DEVNULL

    return subprocess.run(cmd, **kwargs)


def safe_popen(cmd: list[str], **kwargs: Any) -> subprocess.Popen:
    """Execute subprocess.Popen with platform-specific flags.

    Similar to safe_run() but for Popen cases where you need
    the process handle for long-running operations.

    Automatically applies:
    - CREATE_NO_WINDOW on Windows (prevents console window)
    - stdin=DEVNULL (prevents console input handle inheritance)

    Args:
        cmd: Command and arguments (same as subprocess.Popen)
        **kwargs: Additional arguments passed to subprocess.Popen

    Returns:
        Popen process handle

    Note:
        - If 'creationflags' is explicitly provided in kwargs,
          it will be OR'd with platform defaults to preserve custom flags.
        - If 'stdin' is explicitly provided in kwargs, it will be used as-is.
          Otherwise, stdin is automatically redirected to subprocess.DEVNULL.
    """
    default_flags = get_subprocess_creation_flags()

    if "creationflags" in kwargs:
        kwargs["creationflags"] = kwargs["creationflags"] | default_flags
    elif default_flags:
        kwargs["creationflags"] = default_flags

    # Auto-redirect stdin to prevent console input handle inheritance
    # This prevents child processes from stealing keystrokes on Windows
    if "stdin" not in kwargs:
        kwargs["stdin"] = subprocess.DEVNULL

    return subprocess.Popen(cmd, **kwargs)
