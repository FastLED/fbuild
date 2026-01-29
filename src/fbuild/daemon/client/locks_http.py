"""
Lock Management - HTTP Version

Handles daemon lock status queries and stale lock cleanup via HTTP.
"""

from typing import Any

from .http_utils import get_daemon_url, http_client


def daemon_http_request(
    method: str,
    endpoint: str,
    json_data: dict[str, Any] | None = None,
    timeout: float = 30.0,
) -> dict[str, Any] | None:
    """Make an HTTP request to the daemon.

    Args:
        method: HTTP method (GET, POST, etc.)
        endpoint: API endpoint path
        json_data: Optional JSON data for request body
        timeout: Request timeout in seconds

    Returns:
        Response JSON dictionary, or None if request failed
    """
    try:
        url = get_daemon_url(endpoint)
        with http_client(timeout=timeout) as client:
            if method.upper() == "GET":
                response = client.get(url)
            elif method.upper() == "POST":
                response = client.post(url, json=json_data)
            elif method.upper() == "PUT":
                response = client.put(url, json=json_data)
            elif method.upper() == "DELETE":
                response = client.delete(url)
            else:
                return None

            if response.status_code >= 400:
                return None

            return response.json()

    except KeyboardInterrupt:
        raise
    except Exception:
        return None


def get_lock_status_http(timeout: float = 5.0) -> dict[str, Any]:
    """Get current lock status from the daemon via HTTP.

    Shows which ports and projects have active locks and who holds them.

    Args:
        timeout: Request timeout in seconds.

    Returns:
        Dictionary with lock status information:
        - port_locks: Dict of port -> lock info
        - project_locks: Dict of project -> lock info
        - stale_locks: List of locks that appear to be stale

    Example:
        >>> status = get_lock_status_http()
        >>> for port, info in status["port_locks"].items():
        ...     if info.get("is_held"):
        ...         print(f"Port {port} locked by: {info.get('holder_description')}")
    """
    response = daemon_http_request(
        method="GET",
        endpoint="/api/locks/status",
        timeout=timeout,
    )

    if response is None:
        return {
            "port_locks": {},
            "project_locks": {},
            "stale_locks": [],
        }

    return {
        "port_locks": response.get("port_locks", {}),
        "project_locks": response.get("project_locks", {}),
        "stale_locks": response.get("stale_locks", []),
    }


def clear_stale_locks_http(timeout: float = 5.0) -> dict[str, Any]:
    """Clear stale locks via HTTP.

    Forces release of any locks that have been held beyond their timeout.
    Useful when operations have hung or crashed without properly releasing locks.

    Args:
        timeout: Request timeout in seconds.

    Returns:
        Response dictionary with success status and cleared_count.

    Example:
        >>> result = clear_stale_locks_http()
        >>> if result.get("success"):
        ...     print(f"Cleared {result['cleared_count']} stale lock(s)")
    """
    response = daemon_http_request(
        method="POST",
        endpoint="/api/locks/clear",
        timeout=timeout,
    )

    if response is None:
        return {
            "success": False,
            "cleared_count": 0,
            "message": "Failed to connect to daemon",
        }

    return response


def display_lock_status_http() -> None:
    """Display current lock status in a human-readable format via HTTP."""
    from .lifecycle import is_daemon_running

    if not is_daemon_running():
        print("Daemon is not running - no active locks")
        return

    lock_status = get_lock_status_http()

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
