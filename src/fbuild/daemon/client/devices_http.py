"""
Device Management - HTTP Version

Handles device discovery, leasing, and status queries via HTTP.
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


def list_devices_http(refresh: bool = False, timeout: float = 5.0) -> list[dict[str, Any]] | None:
    """List all devices known to the daemon via HTTP.

    Args:
        refresh: Whether to refresh device discovery before listing.
        timeout: Request timeout in seconds.

    Returns:
        List of device info dictionaries, or None if daemon not running.
        Each device dict contains:
        - device_id: Stable device identifier
        - port: Current port (may change)
        - is_connected: Whether device is currently connected
        - exclusive_holder: Client ID holding exclusive lease (or None)
        - monitor_count: Number of active monitor leases
    """
    response = daemon_http_request(
        method="POST",
        endpoint="/api/devices/list",
        json_data={"refresh": refresh},
        timeout=timeout,
    )

    if response is None:
        return None

    if response.get("success"):
        return response.get("devices", [])

    return []


def get_device_status_http(device_id: str, timeout: float = 5.0) -> dict[str, Any] | None:
    """Get detailed status for a specific device via HTTP.

    Args:
        device_id: The device ID to query.
        timeout: Request timeout in seconds.

    Returns:
        Device status dictionary, or None if device not found or daemon not running.
    """
    response = daemon_http_request(
        method="GET",
        endpoint=f"/api/devices/{device_id}/status",
        timeout=timeout,
    )

    if response is None:
        return None

    if response.get("success"):
        return response

    return None


def acquire_device_lease_http(
    device_id: str,
    lease_type: str = "exclusive",
    description: str = "",
    timeout: float = 5.0,
) -> dict[str, Any] | None:
    """Acquire a lease on a device via HTTP.

    Args:
        device_id: The device ID to lease.
        lease_type: Type of lease - "exclusive" or "monitor".
        description: Description of the operation.
        timeout: Request timeout in seconds.

    Returns:
        Response dictionary with success status and lease_id, or None if failed.
    """
    response = daemon_http_request(
        method="POST",
        endpoint=f"/api/devices/{device_id}/lease",
        json_data={
            "lease_type": lease_type,
            "description": description,
        },
        timeout=timeout,
    )

    return response


def release_device_lease_http(device_id: str, timeout: float = 5.0) -> dict[str, Any] | None:
    """Release a lease on a device via HTTP.

    Args:
        device_id: The device ID or lease ID to release.
        timeout: Request timeout in seconds.

    Returns:
        Response dictionary with success status, or None if failed.
    """
    response = daemon_http_request(
        method="POST",
        endpoint=f"/api/devices/{device_id}/release",
        timeout=timeout,
    )

    return response


def preempt_device_http(device_id: str, reason: str, timeout: float = 5.0) -> dict[str, Any] | None:
    """Preempt a device from its current holder via HTTP.

    Args:
        device_id: The device ID to preempt.
        reason: Reason for preemption (required).
        timeout: Request timeout in seconds.

    Returns:
        Response dictionary with success status and preempted_client_id, or None if failed.
    """
    if not reason:
        return {"success": False, "message": "Reason is required for preemption"}

    response = daemon_http_request(
        method="POST",
        endpoint=f"/api/devices/{device_id}/preempt",
        json_data={"reason": reason},
        timeout=timeout,
    )

    return response
