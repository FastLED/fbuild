"""
Daemon singleton manager: Atomic spawn and lock management.

This module handles the low-level mechanics of ensuring only ONE daemon
process exists at any time, using platform-specific locking.
"""

import contextlib
import os
import platform
import subprocess
import sys
import time
from typing import Any, Generator

from fbuild.daemon.paths import DAEMON_DIR, LOCK_FILE, PID_FILE
from fbuild.subprocess_utils import safe_popen, safe_run

# Platform-specific imports
fcntl: Any = None
if platform.system() != "Windows":
    import fcntl


@contextlib.contextmanager
def atomic_singleton_lock() -> Generator[int, None, None]:
    """
    Acquire atomic singleton lock for daemon spawn.

    Uses atomic PID file creation with retry logic on Windows to avoid
    msvcrt deadlock issues. Uses fcntl on Unix.

    Lock is held until context manager exits.

    Yields:
        File descriptor for lock file (or 0 on Windows with PID-based locking)
    """
    # Ensure daemon directory exists
    DAEMON_DIR.mkdir(parents=True, exist_ok=True)

    if platform.system() == "Windows":
        # Windows: Use atomic PID file approach to avoid msvcrt deadlock
        # Strategy: Repeatedly try to create lock file exclusively (O_EXCL)
        # If it fails (FileExistsError), another process holds the lock - wait and retry
        max_retries = 50  # Total wait: 5 seconds max
        retry_delay = 0.1  # 100ms per retry

        for attempt in range(max_retries):
            try:
                # Try to create lock file exclusively (atomic operation)
                lock_fd = os.open(str(LOCK_FILE), os.O_CREAT | os.O_EXCL | os.O_RDWR)
                # Success! We own the lock
                try:
                    # Double-check daemon isn't already running (race condition protection)
                    # Read PID file directly to avoid recursion
                    daemon_pid = None
                    if PID_FILE.exists():
                        try:
                            pid_str = PID_FILE.read_text().strip()
                            daemon_pid = int(pid_str.split(",")[0])
                        except (ValueError, IndexError, FileNotFoundError):
                            daemon_pid = None

                    if daemon_pid is not None and _check_pid_alive(daemon_pid):
                        # Another process won the race and started daemon
                        # Just return without spawning
                        yield 0
                    else:
                        # We need to spawn daemon
                        yield lock_fd
                finally:
                    try:
                        os.close(lock_fd)
                    except OSError:
                        pass  # Already closed
                    # Clean up lock file
                    if LOCK_FILE.exists():
                        try:
                            LOCK_FILE.unlink()
                        except KeyboardInterrupt:
                            raise
                        except Exception:
                            pass  # Ignore cleanup errors
                return  # Exit context manager

            except FileExistsError:
                # Lock file already exists - another process has the lock
                # Wait and retry (blocking behavior)
                if attempt < max_retries - 1:
                    time.sleep(retry_delay)
                else:
                    # Failed to acquire lock after all retries
                    # Check if daemon is running - if so, that's OK
                    # Read PID directly to avoid recursion
                    daemon_pid = None
                    if PID_FILE.exists():
                        try:
                            pid_str = PID_FILE.read_text().strip()
                            daemon_pid = int(pid_str.split(",")[0])
                        except (ValueError, IndexError, FileNotFoundError):
                            daemon_pid = None

                    if daemon_pid is not None and _check_pid_alive(daemon_pid):
                        # Daemon is running, no problem
                        yield 0
                        return
                    else:
                        # Lock exists but no daemon - this is an error state (stale lock)
                        # Try to clean up stale lock
                        try:
                            LOCK_FILE.unlink()
                        except KeyboardInterrupt:
                            raise
                        except Exception:
                            pass
                        raise RuntimeError(f"Failed to acquire daemon lock after {max_retries * retry_delay:.1f}s. Lock file exists but daemon is not running (stale lock).")

    else:
        # Unix: use fcntl advisory locking
        lock_fd = os.open(str(LOCK_FILE), os.O_CREAT | os.O_RDWR, 0o644)
        fcntl.flock(lock_fd, fcntl.LOCK_EX)  # Blocking wait

        try:
            yield lock_fd
        finally:
            fcntl.flock(lock_fd, fcntl.LOCK_UN)
            os.close(lock_fd)
            if LOCK_FILE.exists():
                try:
                    LOCK_FILE.unlink()
                except KeyboardInterrupt:
                    raise
                except Exception:
                    pass  # Ignore cleanup errors


def cleanup_zombie_daemons() -> None:
    """Clean up zombie daemon processes that are holding ports but not responding via HTTP.

    This function:
    1. Checks if a daemon is HTTP-available - if so, does nothing
    2. Otherwise, kills any processes holding daemon ports
    3. Removes stale PID files

    This prevents port conflicts when spawning new daemons while preserving healthy ones.
    """
    import logging

    # First check if there's a healthy daemon - if so, don't clean anything
    from fbuild.daemon.client.http_utils import is_daemon_http_available

    if is_daemon_http_available():
        logging.debug("Daemon is HTTP-available, skipping zombie cleanup")
        return

    # Determine ports based on dev mode
    is_dev = os.environ.get("FBUILD_DEV_MODE") == "1"
    http_port = 8865 if is_dev else 8765
    ws_port = 9876  # Same for both modes

    killed_any = False

    # Find and kill processes holding the HTTP port
    if sys.platform == "win32":
        try:
            result = subprocess.run(
                ["netstat", "-ano"],
                capture_output=True,
                text=True,
                check=False,
            )
            for line in result.stdout.splitlines():
                if f":{http_port}" in line or f":{ws_port}" in line:
                    if "LISTENING" in line:
                        parts = line.split()
                        if parts:
                            try:
                                pid = int(parts[-1])
                                logging.warning(f"Killing zombie daemon PID {pid} holding port")
                                subprocess.run(
                                    ["taskkill", "/F", "/PID", str(pid)],
                                    capture_output=True,
                                    check=False,
                                )
                                killed_any = True
                            except (ValueError, IndexError):
                                pass
        except Exception as e:
            logging.debug(f"Error during zombie cleanup: {e}")
    else:
        # Unix-like systems
        for port in [http_port, ws_port]:
            try:
                result = subprocess.run(
                    ["lsof", "-ti", f":{port}"],
                    capture_output=True,
                    text=True,
                    check=False,
                )
                for pid_str in result.stdout.strip().split():
                    try:
                        pid = int(pid_str)
                        logging.warning(f"Killing zombie daemon PID {pid} holding port {port}")
                        os.kill(pid, 9)
                        killed_any = True
                    except (ValueError, ProcessLookupError):
                        pass
            except Exception as e:
                logging.debug(f"Error during zombie cleanup for port {port}: {e}")

    # Clean stale PID file if we killed anything
    if killed_any and PID_FILE.exists():
        try:
            PID_FILE.unlink()
            logging.info("Removed stale PID file after zombie cleanup")
        except Exception:
            pass

    # Give killed processes time to release ports
    if killed_any:
        time.sleep(0.5)


def spawn_daemon_process(launcher_pid: int) -> int:
    """
    Spawn daemon process and return its PID.

    Args:
        launcher_pid: PID of the client that requested daemon

    Returns:
        PID of spawned daemon process

    Raises:
        RuntimeError: If spawn fails
    """
    # Clean up any zombie daemons before spawning
    cleanup_zombie_daemons()

    # Construct daemon command
    # TEMPORARY: Use sys.executable for debugging (will show console)
    # TODO: Switch back to get_python_executable() once daemon spawn is working
    cmd = [
        sys.executable,  # python.exe for debugging
        "-m",
        "fbuild.daemon.daemon",
        f"--launched-by={launcher_pid}",  # Must use = form, not separate args
    ]

    # Spawn daemon
    # safe_popen() automatically handles:
    # - CREATE_NO_WINDOW on Windows (prevents console window)
    # - stdin=DEVNULL (prevents console input handle inheritance)
    # TEMPORARY: Capture stderr for debugging
    # NOTE: We don't change cwd - daemon determines its own paths based on FBUILD_DEV_MODE

    # Ensure environment is passed (especially FBUILD_DEV_MODE)
    env = os.environ.copy()

    # On Windows, passing file handles to subprocess can cause issues with append mode.
    # Instead, we let the daemon itself handle logging by redirecting stderr to DEVNULL
    # and having the daemon write its startup info to the log file.
    #
    # This approach is more reliable because:
    # 1. Each daemon process explicitly opens the file in append mode
    # 2. No file handle inheritance issues on Windows
    # 3. Clean separation between parent and child file handles

    # Use safe_popen to avoid console window issues on Windows
    proc = safe_popen(
        cmd,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,  # Daemon will handle its own stderr logging
        stdin=subprocess.DEVNULL,
        env=env,  # Explicitly pass environment
    )

    return proc.pid


def wait_for_pid_file(expected_pid: int, timeout: float = 10.0) -> int:
    """
    Wait for daemon to write PID file.

    This function accepts ANY alive daemon PID, not just the expected one.
    This tolerates race conditions where:
    - Expected PID crashes immediately before writing PID file
    - Concurrent spawn succeeds with different PID
    - Windows process creation returns PID before full initialization

    Args:
        expected_pid: PID we expect to see (for logging purposes)
        timeout: Maximum seconds to wait

    Returns:
        Actual daemon PID that successfully wrote the PID file

    Raises:
        TimeoutError: If no daemon writes PID file within timeout
    """
    import logging

    deadline = time.time() + timeout

    while time.time() < deadline:
        if PID_FILE.exists():
            try:
                pid_str = PID_FILE.read_text().strip()
                pid = int(pid_str.split(",")[0])  # Format: "pid,launcher_pid"

                # Check if this daemon is alive
                if _check_pid_alive(pid):
                    if pid != expected_pid:
                        logging.warning(f"Expected PID {expected_pid}, but PID {pid} started successfully (concurrent spawn or immediate crash + retry). Accepting alive daemon.")
                    return pid  # ✅ Accept any alive daemon
                else:
                    # PID file contains dead process - keep waiting
                    pass
            except (ValueError, IndexError):
                # Corrupt PID file, keep waiting
                pass

        time.sleep(0.1)

    raise TimeoutError(f"No daemon wrote PID file within {timeout}s (expected PID {expected_pid})")


def _check_pid_alive(pid: int) -> bool:
    """
    Check if a specific PID is alive (low-level, no file I/O).

    Args:
        pid: Process ID to check

    Returns:
        True if process exists, False otherwise
    """
    try:
        if platform.system() == "Windows":
            result = safe_run(
                ["tasklist", "/FI", f"PID eq {pid}", "/NH"],
                capture_output=True,
                text=True,
            )
            return str(pid) in result.stdout
        else:
            os.kill(pid, 0)  # Signal 0: check if process exists
            return True
    except (ProcessLookupError, PermissionError):
        return False


def is_daemon_alive() -> bool:
    """
    Check if daemon is actually running.

    Returns:
        True if daemon process is alive, False otherwise
    """
    if not PID_FILE.exists():
        return False

    try:
        pid_str = PID_FILE.read_text().strip()
        pid = int(pid_str.split(",")[0])
        return _check_pid_alive(pid)
    except (ValueError, IndexError, FileNotFoundError):
        return False


def read_pid_file() -> int | None:
    """
    Read daemon PID from PID file.

    Returns:
        Daemon PID, or None if file doesn't exist or is corrupt
    """
    if not PID_FILE.exists():
        return None

    try:
        pid_str = PID_FILE.read_text().strip()
        pid = int(pid_str.split(",")[0])
        return pid
    except (ValueError, IndexError):
        return None


def get_launcher_pid() -> int | None:
    """
    Read launcher PID from PID file.

    Returns:
        Launcher PID, or None if file doesn't exist or is corrupt
    """
    if not PID_FILE.exists():
        return None

    try:
        pid_str = PID_FILE.read_text().strip()
        parts = pid_str.split(",")
        if len(parts) >= 2:
            return int(parts[1])
        return None
    except (ValueError, IndexError):
        return None
