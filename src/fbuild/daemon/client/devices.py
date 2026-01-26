"""
Device Management

Handles device discovery, leasing, and status queries.
"""

import json
import time
from typing import Any

from .lifecycle import DAEMON_DIR, is_daemon_running


def list_devices(refresh: bool = False) -> list[dict[str, Any]] | None:
    """List all devices known to the daemon.

    Args:
        refresh: Whether to refresh device discovery before listing.

    Returns:
        List of device info dictionaries, or None if daemon not running.
        Each device dict contains:
        - device_id: Stable device identifier
        - port: Current port (may change)
        - is_connected: Whether device is currently connected
        - exclusive_holder: Client ID holding exclusive lease (or None)
        - monitor_count: Number of active monitor leases
    """
    if not is_daemon_running():
        return None

    # For now, we use a signal file to communicate with the daemon
    # In the future, this should use the async TCP connection
    request_file = DAEMON_DIR / "device_list_request.json"
    response_file = DAEMON_DIR / "device_list_response.json"

    # Clean up any old response file
    response_file.unlink(missing_ok=True)

    # Write request
    request = {"refresh": refresh, "timestamp": time.time()}
    with open(request_file, "w") as f:
        json.dump(request, f)

    # Wait for response (timeout 5 seconds)
    for _ in range(50):
        if response_file.exists():
            try:
                with open(response_file) as f:
                    response = json.load(f)
                response_file.unlink(missing_ok=True)
                if response.get("success"):
                    return response.get("devices", [])
                return []
            except (json.JSONDecodeError, OSError):
                pass
        time.sleep(0.1)

    # Timeout - clean up
    request_file.unlink(missing_ok=True)
    return None


def get_device_status(device_id: str) -> dict[str, Any] | None:
    """Get detailed status for a specific device.

    Args:
        device_id: The device ID to query.

    Returns:
        Device status dictionary, or None if device not found or daemon not running.
    """
    if not is_daemon_running():
        return None

    request_file = DAEMON_DIR / "device_status_request.json"
    response_file = DAEMON_DIR / "device_status_response.json"

    # Clean up any old response file
    response_file.unlink(missing_ok=True)

    # Write request
    request = {"device_id": device_id, "timestamp": time.time()}
    with open(request_file, "w") as f:
        json.dump(request, f)

    # Wait for response
    for _ in range(50):
        if response_file.exists():
            try:
                with open(response_file) as f:
                    response = json.load(f)
                response_file.unlink(missing_ok=True)
                if response.get("success"):
                    return response
                return None
            except (json.JSONDecodeError, OSError):
                pass
        time.sleep(0.1)

    request_file.unlink(missing_ok=True)
    return None


def acquire_device_lease(
    device_id: str,
    lease_type: str = "exclusive",
    description: str = "",
) -> dict[str, Any] | None:
    """Acquire a lease on a device.

    Args:
        device_id: The device ID to lease.
        lease_type: Type of lease - "exclusive" or "monitor".
        description: Description of the operation.

    Returns:
        Response dictionary with success status and lease_id, or None if failed.
    """
    if not is_daemon_running():
        return None

    request_file = DAEMON_DIR / "device_lease_request.json"
    response_file = DAEMON_DIR / "device_lease_response.json"

    response_file.unlink(missing_ok=True)

    request = {
        "device_id": device_id,
        "lease_type": lease_type,
        "description": description,
        "timestamp": time.time(),
    }
    with open(request_file, "w") as f:
        json.dump(request, f)

    for _ in range(50):
        if response_file.exists():
            try:
                with open(response_file) as f:
                    response = json.load(f)
                response_file.unlink(missing_ok=True)
                return response
            except (json.JSONDecodeError, OSError):
                pass
        time.sleep(0.1)

    request_file.unlink(missing_ok=True)
    return None


def release_device_lease(device_id: str) -> dict[str, Any] | None:
    """Release a lease on a device.

    Args:
        device_id: The device ID or lease ID to release.

    Returns:
        Response dictionary with success status, or None if failed.
    """
    if not is_daemon_running():
        return None

    request_file = DAEMON_DIR / "device_release_request.json"
    response_file = DAEMON_DIR / "device_release_response.json"

    response_file.unlink(missing_ok=True)

    request = {"device_id": device_id, "timestamp": time.time()}
    with open(request_file, "w") as f:
        json.dump(request, f)

    for _ in range(50):
        if response_file.exists():
            try:
                with open(response_file) as f:
                    response = json.load(f)
                response_file.unlink(missing_ok=True)
                return response
            except (json.JSONDecodeError, OSError):
                pass
        time.sleep(0.1)

    request_file.unlink(missing_ok=True)
    return None


def preempt_device(device_id: str, reason: str) -> dict[str, Any] | None:
    """Preempt a device from its current holder.

    Args:
        device_id: The device ID to preempt.
        reason: Reason for preemption (required).

    Returns:
        Response dictionary with success status and preempted_client_id, or None if failed.
    """
    if not is_daemon_running():
        return None

    if not reason:
        return {"success": False, "message": "Reason is required for preemption"}

    request_file = DAEMON_DIR / "device_preempt_request.json"
    response_file = DAEMON_DIR / "device_preempt_response.json"

    response_file.unlink(missing_ok=True)

    request = {"device_id": device_id, "reason": reason, "timestamp": time.time()}
    with open(request_file, "w") as f:
        json.dump(request, f)

    for _ in range(50):
        if response_file.exists():
            try:
                with open(response_file) as f:
                    response = json.load(f)
                response_file.unlink(missing_ok=True)
                return response
            except (json.JSONDecodeError, OSError):
                pass
        time.sleep(0.1)

    request_file.unlink(missing_ok=True)
    return None
