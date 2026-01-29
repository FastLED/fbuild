"""
Daemon Lifecycle Management

Handles daemon process startup, shutdown, and health checks.

REFACTORED: Uses centralized daemon API for spawn management and HTTP-based
communication with the FastAPI daemon. All spawn logic is now in fbuild.daemon.api
and fbuild.daemon.singleton_manager.
"""

import time

from fbuild.daemon.api import DaemonStatus, get_daemon_info, request_daemon
from fbuild.daemon.client.http_utils import (
    get_daemon_url,
    http_client,
    is_daemon_http_available,
)
from fbuild.daemon.paths import (
    BUILD_REQUEST_FILE,
    DAEMON_DIR,
    DAEMON_NAME,
    DEPLOY_REQUEST_FILE,
    INSTALL_DEPS_REQUEST_FILE,
    MONITOR_REQUEST_FILE,
    PID_FILE,
    SERIAL_MONITOR_ATTACH_REQUEST_FILE,
    SERIAL_MONITOR_DETACH_REQUEST_FILE,
    SERIAL_MONITOR_POLL_REQUEST_FILE,
    SERIAL_MONITOR_RESPONSE_FILE,
    STATUS_FILE,
)

# Re-export for backward compatibility
__all__ = [
    "DAEMON_DIR",
    "DAEMON_NAME",
    "PID_FILE",
    "STATUS_FILE",
    "BUILD_REQUEST_FILE",
    "DEPLOY_REQUEST_FILE",
    "MONITOR_REQUEST_FILE",
    "INSTALL_DEPS_REQUEST_FILE",
    "SERIAL_MONITOR_ATTACH_REQUEST_FILE",
    "SERIAL_MONITOR_DETACH_REQUEST_FILE",
    "SERIAL_MONITOR_POLL_REQUEST_FILE",
    "SERIAL_MONITOR_RESPONSE_FILE",
    "is_daemon_running",
    "ensure_daemon_running",
    "stop_daemon",
]


def is_daemon_running() -> bool:
    """
    Check if daemon is running (no spawn attempt).

    Returns:
        True if daemon is running, False otherwise
    """
    response = get_daemon_info()
    return response.status == DaemonStatus.ALREADY_RUNNING


def ensure_daemon_running(timeout: int = 10, verbose: bool = False) -> None:
    """
    Ensure daemon is running. Idempotent and thread-safe.

    This function delegates to the daemon API layer, which handles all
    spawn logic, locking, and race condition prevention. Uses HTTP health
    checks to verify daemon readiness.

    Args:
        timeout: Maximum seconds to wait for daemon readiness
        verbose: Whether to print status messages

    Raises:
        RuntimeError: If daemon cannot be started or become ready within timeout
    """
    if verbose:
        print("🔗 Requesting daemon...")

    # Request daemon (spawn if needed)
    response = request_daemon()

    if response.status == DaemonStatus.STARTED:
        if verbose:
            print(f"✅ Daemon started (PID {response.pid})")
        # Wait for daemon HTTP server to be ready (already done in request_daemon)
        # Double-check it's still available
        if not is_daemon_http_available():
            # Rare case: daemon died immediately after starting
            raise RuntimeError(f"Daemon started but HTTP server became unavailable. Check daemon logs at: {DAEMON_DIR / 'daemon.log'}")
        if verbose:
            print("✅ Daemon HTTP server is ready")
        return

    elif response.status == DaemonStatus.ALREADY_RUNNING:
        if verbose:
            print(f"✅ Daemon already running (PID {response.pid}, launched by PID {response.launched_by})")
        # Daemon already running - no need to wait
        return

    else:  # FAILED
        raise RuntimeError(f"Failed to start daemon: {response.message}")


def stop_daemon() -> bool:
    """Stop the daemon gracefully via HTTP shutdown endpoint.

    Returns:
        True if daemon was stopped, False otherwise
    """
    if not is_daemon_running():
        print("Daemon is not running")
        return False

    # Send HTTP shutdown request
    try:
        with http_client(timeout=5.0) as client:
            response = client.post(get_daemon_url("/api/daemon/shutdown"))
            if response.status_code == 200:
                print("Stopping daemon...")
            elif response.status_code == 409:
                print("⚠️  Cannot stop daemon: operation in progress")
                return False
            else:
                print(f"⚠️  Shutdown request returned status {response.status_code}")
                # Fall back to signal file for backward compatibility
                shutdown_file = DAEMON_DIR / "shutdown.signal"
                shutdown_file.touch()
                print("Stopping daemon (fallback signal)...")
    except KeyboardInterrupt:
        raise
    except Exception as e:
        print(f"⚠️  HTTP shutdown failed: {e}")
        # Fall back to signal file
        shutdown_file = DAEMON_DIR / "shutdown.signal"
        shutdown_file.touch()
        print("Stopping daemon (fallback signal)...")

    # Wait for daemon to exit (check HTTP availability)
    for _ in range(10):
        if not is_daemon_running():
            print("✅ Daemon stopped")
            return True
        time.sleep(1)

    print("⚠️  Daemon did not stop gracefully")
    return False
