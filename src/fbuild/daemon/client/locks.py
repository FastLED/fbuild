"""
Lock Management

Handles daemon lock status queries and stale lock cleanup.
"""

from typing import Any

from .lifecycle import DAEMON_DIR, is_daemon_running
from .status import read_status_file


def get_lock_status() -> dict[str, Any]:
    """Get current lock status from the daemon.

    Reads the daemon status file and extracts lock information.
    This shows which ports and projects have active locks and who holds them.

    Returns:
        Dictionary with lock status information:
        - port_locks: Dict of port -> lock info
        - project_locks: Dict of project -> lock info
        - stale_locks: List of locks that appear to be stale

    Example:
        >>> status = get_lock_status()
        >>> for port, info in status["port_locks"].items():
        ...     if info.get("is_held"):
        ...         print(f"Port {port} locked by: {info.get('holder_description')}")
    """
    status = read_status_file()
    locks = status.locks if hasattr(status, "locks") and status.locks else {}

    # Extract stale locks
    stale_locks: list[dict[str, Any]] = []

    port_locks = locks.get("port_locks", {})
    for port, info in port_locks.items():
        if isinstance(info, dict) and info.get("is_stale"):
            stale_locks.append(
                {
                    "type": "port",
                    "resource": port,
                    "holder": info.get("holder_description"),
                    "hold_duration": info.get("hold_duration"),
                }
            )

    project_locks = locks.get("project_locks", {})
    for project, info in project_locks.items():
        if isinstance(info, dict) and info.get("is_stale"):
            stale_locks.append(
                {
                    "type": "project",
                    "resource": project,
                    "holder": info.get("holder_description"),
                    "hold_duration": info.get("hold_duration"),
                }
            )

    return {
        "port_locks": port_locks,
        "project_locks": project_locks,
        "stale_locks": stale_locks,
    }


def request_clear_stale_locks() -> bool:
    """Request the daemon to clear stale locks.

    Sends a signal to the daemon to force-release any locks that have been
    held beyond their timeout. This is useful when operations have hung or
    crashed without properly releasing their locks.

    Returns:
        True if signal was sent, False if daemon not running

    Note:
        The daemon checks for stale locks periodically (every 60 seconds).
        This function triggers an immediate check by writing a signal file.
        The actual clearing happens on the daemon side.
    """
    if not is_daemon_running():
        print("Daemon is not running")
        return False

    # Create signal file to trigger stale lock cleanup
    signal_file = DAEMON_DIR / "clear_stale_locks.signal"
    DAEMON_DIR.mkdir(parents=True, exist_ok=True)
    signal_file.touch()

    print("Signal sent to daemon to clear stale locks")
    print("Note: Check status with 'fbuild daemon status' to see results")
    return True


def display_lock_status() -> None:
    """Display current lock status in a human-readable format."""
    if not is_daemon_running():
        print("Daemon is not running - no active locks")
        return

    lock_status = get_lock_status()

    print("\n=== Lock Status ===\n")

    # Port locks
    port_locks = lock_status.get("port_locks", {})
    if port_locks:
        print("Port Locks:")
        for port, info in port_locks.items():
            if isinstance(info, dict):
                held = info.get("is_held", False)
                stale = info.get("is_stale", False)
                holder = info.get("holder_description", "unknown")
                duration = info.get("hold_duration")

                status_str = "FREE"
                if held:
                    status_str = "STALE" if stale else "HELD"

                duration_str = f" ({duration:.1f}s)" if duration else ""
                holder_str = f" by {holder}" if held else ""

                print(f"  {port}: {status_str}{holder_str}{duration_str}")
    else:
        print("Port Locks: (none)")

    # Project locks
    project_locks = lock_status.get("project_locks", {})
    if project_locks:
        print("\nProject Locks:")
        for project, info in project_locks.items():
            if isinstance(info, dict):
                held = info.get("is_held", False)
                stale = info.get("is_stale", False)
                holder = info.get("holder_description", "unknown")
                duration = info.get("hold_duration")

                status_str = "FREE"
                if held:
                    status_str = "STALE" if stale else "HELD"

                duration_str = f" ({duration:.1f}s)" if duration else ""
                holder_str = f" by {holder}" if held else ""

                # Truncate long project paths
                display_project = project
                if len(project) > 50:
                    display_project = "..." + project[-47:]

                print(f"  {display_project}: {status_str}{holder_str}{duration_str}")
    else:
        print("\nProject Locks: (none)")

    # Stale locks warning
    stale_locks = lock_status.get("stale_locks", [])
    if stale_locks:
        print(f"\n⚠️  Found {len(stale_locks)} stale lock(s)!")
        print("   Use 'fbuild daemon clear-locks' to force-release them")

    print()
