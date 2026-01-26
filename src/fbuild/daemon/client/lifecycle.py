"""
Daemon Lifecycle Management

Handles daemon process startup, shutdown, and health checks.
"""

import os
import subprocess
import sys
import time
from pathlib import Path

import psutil

from fbuild.daemon.messages import DaemonState

from ...subprocess_utils import get_python_executable, safe_popen

# Daemon configuration (must match daemon settings)
DAEMON_NAME = "fbuild_daemon"

# Check for development mode (when running from repo)
if os.environ.get("FBUILD_DEV_MODE") == "1":
    # Use project-local daemon directory for development
    DAEMON_DIR = Path.cwd() / ".fbuild" / "daemon_dev"
else:
    # Use home directory for production
    DAEMON_DIR = Path.home() / ".fbuild" / "daemon"

PID_FILE = DAEMON_DIR / f"{DAEMON_NAME}.pid"
STATUS_FILE = DAEMON_DIR / "daemon_status.json"
BUILD_REQUEST_FILE = DAEMON_DIR / "build_request.json"
DEPLOY_REQUEST_FILE = DAEMON_DIR / "deploy_request.json"
MONITOR_REQUEST_FILE = DAEMON_DIR / "monitor_request.json"
INSTALL_DEPS_REQUEST_FILE = DAEMON_DIR / "install_deps_request.json"

# Serial Monitor API request files (used by fbuild.api.SerialMonitor)
SERIAL_MONITOR_ATTACH_REQUEST_FILE = DAEMON_DIR / "serial_monitor_attach_request.json"
SERIAL_MONITOR_DETACH_REQUEST_FILE = DAEMON_DIR / "serial_monitor_detach_request.json"
SERIAL_MONITOR_POLL_REQUEST_FILE = DAEMON_DIR / "serial_monitor_poll_request.json"
SERIAL_MONITOR_RESPONSE_FILE = DAEMON_DIR / "serial_monitor_response.json"


def is_daemon_running() -> bool:
    """Check if daemon is running, clean up stale PID files.

    Returns:
        True if daemon is running, False otherwise
    """
    if not PID_FILE.exists():
        return False

    try:
        with open(PID_FILE) as f:
            pid = int(f.read().strip())

        # Check if process exists
        if psutil.pid_exists(pid):
            return True
        else:
            # Stale PID file - remove it
            print(f"Removing stale PID file: {PID_FILE}")
            PID_FILE.unlink()
            return False
    except KeyboardInterrupt:
        import _thread

        _thread.interrupt_main()
        raise
    except Exception:
        # Corrupted PID file - remove it
        try:
            PID_FILE.unlink(missing_ok=True)
        except KeyboardInterrupt:
            import _thread

            _thread.interrupt_main()
            raise
        except Exception:
            pass
        return False


def start_daemon() -> None:
    """Start the daemon process.

    Passes the spawning client's PID as an argument so the daemon can log
    which client originally started it.

    On Windows, uses proper detachment flags to ensure:
    - Daemon survives client termination (DETACHED_PROCESS)
    - Daemon is isolated from client's Ctrl-C signals (CREATE_NEW_PROCESS_GROUP)
    """
    # Pass spawning client PID so daemon can log who started it
    spawner_pid = os.getpid()

    # On Windows, use proper detachment flags:
    # - CREATE_NEW_PROCESS_GROUP: Isolates daemon from client's Ctrl-C signals
    # - DETACHED_PROCESS: Daemon survives client termination, no console inherited
    creationflags = 0
    if sys.platform == "win32":
        creationflags = subprocess.CREATE_NEW_PROCESS_GROUP | subprocess.DETACHED_PROCESS

    # Start daemon in background as a fully detached process
    # Use -m to run as module (required for relative imports in daemon.py)
    safe_popen(
        [get_python_executable(), "-m", "fbuild.daemon.daemon", f"--spawned-by={spawner_pid}"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        stdin=subprocess.DEVNULL,
        creationflags=creationflags,
    )


def ensure_daemon_running() -> None:
    """Ensure daemon is running, start if needed.

    Raises:
        RuntimeError: If daemon cannot be started within 10 seconds
    """
    # Import here to avoid circular dependency
    from .status import read_status_file

    daemon_needs_restart = False

    if is_daemon_running():
        # Daemon PID exists - but verify it's actually responsive
        # Check status file to ensure daemon isn't shutting down
        status = read_status_file()
        if status.state != DaemonState.UNKNOWN:
            # Check if daemon is shutting down (state=IDLE with shutdown message)
            if status.state == DaemonState.IDLE and "shut down" in status.message.lower():
                # Daemon is shutting down - wait briefly for it to exit
                for _ in range(6):  # Wait up to 3 seconds
                    time.sleep(0.5)
                    if not is_daemon_running():
                        daemon_needs_restart = True
                        break
                # If daemon still running after wait, force restart
                if not daemon_needs_restart:
                    # Daemon stuck in shutdown - this shouldn't happen but handle it
                    daemon_needs_restart = True
            else:
                # Daemon is running and responsive
                return
        else:
            # Can't read status - daemon might be starting up or in bad state
            # Wait briefly and check again
            time.sleep(0.5)
            if is_daemon_running():
                status = read_status_file()
                if status.state != DaemonState.UNKNOWN:
                    return
            # Status still unknown - need to restart
            daemon_needs_restart = True
    else:
        # Daemon not running
        daemon_needs_restart = True

    # If we reach here, daemon needs to be (re)started
    if not daemon_needs_restart:
        return

    # Clear stale status file to prevent race condition where client reads old status
    # from previous daemon run before new daemon writes fresh status
    if STATUS_FILE.exists():
        try:
            STATUS_FILE.unlink()
        except KeyboardInterrupt:
            import _thread

            _thread.interrupt_main()
            raise
        except Exception:
            pass  # Best effort - continue even if delete fails

    print("🔗 Starting fbuild daemon...")
    start_daemon()

    # Wait up to 10 seconds for daemon to start and write fresh status
    for _ in range(10):
        if is_daemon_running():
            # Daemon is running - check if status file is fresh
            status = read_status_file()
            if status.state != DaemonState.UNKNOWN:
                # Valid status received from new daemon
                print("✅ Daemon started successfully")
                return
        time.sleep(1)

    # Failed to start - this is a critical error
    raise RuntimeError(f"Failed to start fbuild daemon within 10 seconds. Check daemon logs at: {DAEMON_DIR / 'daemon.log'}")


def stop_daemon() -> bool:
    """Stop the daemon gracefully.

    Returns:
        True if daemon was stopped, False otherwise
    """
    if not is_daemon_running():
        print("Daemon is not running")
        return False

    # Create shutdown signal file
    shutdown_file = DAEMON_DIR / "shutdown.signal"
    shutdown_file.touch()

    # Wait for daemon to exit
    print("Stopping daemon...")
    for _ in range(10):
        if not is_daemon_running():
            print("✅ Daemon stopped")
            return True
        time.sleep(1)

    print("⚠️  Daemon did not stop gracefully")
    return False
