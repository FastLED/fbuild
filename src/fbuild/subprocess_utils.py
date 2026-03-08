"""Subprocess utilities for platform-safe process execution.

This module provides wrappers around subprocess module that automatically
apply platform-specific flags to prevent console window flashing on Windows
and set low process priority so builds don't starve the rest of the system.
"""

import os
import subprocess
import sys
from pathlib import Path
from typing import Any


def get_python_executable() -> str:
    """Get the Python executable path, preferring pythonw.exe on Windows.

    On Windows, pythonw.exe runs Python scripts without showing a console window,
    which is more effective than CREATE_NO_WINDOW for preventing window flashing.

    Note: In a venv, pythonw.exe is a launcher stub that re-execs the real
    interpreter. This causes Popen to return the stub's PID while os.getpid()
    inside the child returns the real interpreter's PID. This PID mismatch is
    expected and handled by wait_for_pid_file() accepting any alive daemon PID.

    Returns:
        - Windows: Path to pythonw.exe if it exists, otherwise sys.executable
        - Other platforms: sys.executable
    """
    if sys.platform != "win32":
        return sys.executable

    # Try to find pythonw.exe next to python.exe
    python_path = Path(sys.executable)
    pythonw_path = python_path.parent / "pythonw.exe"

    if pythonw_path.exists():
        return str(pythonw_path)

    # Fallback to regular python.exe
    return sys.executable


def get_subprocess_creation_flags() -> int:
    """Get platform-specific subprocess creation flags.

    Returns:
        - Windows: CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS
        - Other platforms: 0 (no special flags; priority set via preexec_fn)
    """
    if sys.platform == "win32":
        return subprocess.CREATE_NO_WINDOW | subprocess.BELOW_NORMAL_PRIORITY_CLASS
    return 0


def _get_preexec_fn():
    """Get a preexec_fn that lowers child process priority on Unix.

    On Windows, priority is set via creation flags instead (BELOW_NORMAL_PRIORITY_CLASS).
    On Unix, os.nice(10) drops the child to low priority so the system stays responsive.
    preexec_fn is not supported on Windows, so this returns None there.

    Returns:
        Callable for preexec_fn on Unix, None on Windows.
    """
    if sys.platform == "win32" or not hasattr(os, "nice"):
        return None

    def _lower_priority():
        os.nice(10)  # type: ignore[attr-defined]

    return _lower_priority


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

    # Set low priority via preexec_fn on Unix (Windows uses creation flags)
    preexec = _get_preexec_fn()
    if preexec is not None and "preexec_fn" not in kwargs:
        kwargs["preexec_fn"] = preexec

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

    # Set low priority via preexec_fn on Unix (Windows uses creation flags)
    preexec = _get_preexec_fn()
    if preexec is not None and "preexec_fn" not in kwargs:
        kwargs["preexec_fn"] = preexec

    return subprocess.Popen(cmd, **kwargs)
