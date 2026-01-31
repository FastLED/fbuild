"""
Device Management Endpoints

Provides HTTP endpoints for device discovery, leasing, and status queries.
"""

from typing import Any

from fastapi import APIRouter
from pydantic import BaseModel, Field

router = APIRouter(prefix="/api/devices", tags=["devices"])


# Request/Response Models
class DeviceListRequest(BaseModel):
    """Request to list devices."""

    refresh: bool = Field(default=False, description="Whether to refresh device discovery before listing")


class DeviceListResponse(BaseModel):
    """Response containing list of devices."""

    success: bool
    devices: list[dict[str, Any]]


class DeviceStatusResponse(BaseModel):
    """Response containing device status."""

    success: bool
    device_id: str | None = None
    is_connected: bool | None = None
    device_info: dict[str, Any] | None = None
    exclusive_lease: dict[str, Any] | None = None
    monitor_leases: list[dict[str, Any]] | None = None
    monitor_count: int | None = None
    is_available_for_exclusive: bool | None = None
    message: str | None = None


class DeviceLeaseRequest(BaseModel):
    """Request to acquire device lease."""

    lease_type: str = Field(default="exclusive", description="Type of lease - 'exclusive' or 'monitor'")
    description: str = Field(default="", description="Description of the operation")


class DeviceLeaseResponse(BaseModel):
    """Response from lease acquisition."""

    success: bool
    lease_id: str | None = None
    message: str | None = None


class DeviceReleaseResponse(BaseModel):
    """Response from lease release."""

    success: bool
    message: str | None = None


class DevicePreemptRequest(BaseModel):
    """Request to preempt device."""

    reason: str = Field(..., description="Reason for preemption (required)")


class DevicePreemptResponse(BaseModel):
    """Response from device preemption."""

    success: bool
    preempted_client_id: str | None = None
    lease_id: str | None = None
    message: str | None = None


# Endpoints
@router.post("/list", response_model=DeviceListResponse)
async def list_devices(request: DeviceListRequest) -> DeviceListResponse:
    """List all devices known to the daemon.

    Args:
        request: Device list request with optional refresh flag.

    Returns:
        List of device information dictionaries.
    """
    # Get from daemon context
    from ..fastapi_app import get_daemon_context

    context = get_daemon_context()
    device_manager = context.device_manager

    # Optionally refresh device discovery
    if request.refresh:
        device_manager.refresh_devices()

    # Get all devices - returns dict[str, DeviceState]
    devices_dict = device_manager.get_all_devices()
    # Convert to list of dicts
    devices_list = [{"device_id": device_id, **state.__dict__} for device_id, state in devices_dict.items()]

    return DeviceListResponse(success=True, devices=devices_list)


@router.get("/{device_id}/status", response_model=DeviceStatusResponse)
async def get_device_status(device_id: str) -> DeviceStatusResponse:
    """Get detailed status for a specific device.

    Args:
        device_id: The device ID to query.

    Returns:
        Device status information.
    """
    from ..fastapi_app import get_daemon_context

    context = get_daemon_context()
    device_manager = context.device_manager

    status = device_manager.get_device_status(device_id)

    if not status:
        return DeviceStatusResponse(success=False, message=f"Device not found: {device_id}")

    return DeviceStatusResponse(success=True, **status)


@router.post("/{device_id}/lease", response_model=DeviceLeaseResponse)
async def acquire_device_lease(
    device_id: str,
    request: DeviceLeaseRequest,
) -> DeviceLeaseResponse:
    """Acquire a lease on a device.

    Args:
        device_id: The device ID to lease.
        request: Lease request with type and description.
        device_manager: Injected DeviceManager instance from daemon context.

    Returns:
        Response with success status and lease_id.
    """
    from ..fastapi_app import get_daemon_context

    context = get_daemon_context()
    device_manager = context.device_manager

    # Generate a client_id (in real implementation, this should come from auth)
    import uuid

    client_id = f"http_client_{uuid.uuid4().hex[:8]}"

    try:
        if request.lease_type == "exclusive":
            lease = device_manager.acquire_exclusive(device_id=device_id, client_id=client_id, description=request.description)
        elif request.lease_type == "monitor":
            lease = device_manager.acquire_monitor(device_id=device_id, client_id=client_id, description=request.description)
        else:
            return DeviceLeaseResponse(success=False, message=f"Invalid lease type: {request.lease_type}")

        if lease:
            return DeviceLeaseResponse(success=True, lease_id=lease.lease_id, message="Lease acquired successfully")
        else:
            return DeviceLeaseResponse(success=False, message="Failed to acquire lease (device unavailable or already leased)")

    except KeyboardInterrupt:
        raise
    except Exception as e:
        return DeviceLeaseResponse(success=False, message=str(e))


@router.post("/{device_id}/release", response_model=DeviceReleaseResponse)
async def release_device_lease(
    device_id: str,
) -> DeviceReleaseResponse:
    """Release a lease on a device.

    Args:
        device_id: The device ID or lease ID to release.
        device_manager: Injected DeviceManager instance from daemon context.

    Returns:
        Response with success status.
    """
    from ..fastapi_app import get_daemon_context

    context = get_daemon_context()
    device_manager = context.device_manager

    try:
        # Generate a client_id (in real implementation, this should come from auth)
        import uuid

        client_id = f"http_client_{uuid.uuid4().hex[:8]}"

        # Try releasing by lease_id
        if device_manager.release_lease(lease_id=device_id, client_id=client_id):
            return DeviceReleaseResponse(success=True, message="Lease released successfully")

        # If that failed, it might be a device_id - try releasing all leases for the device
        # Note: This is a simplified implementation. In production, you'd track which
        # client is making the request and only release their leases.
        return DeviceReleaseResponse(success=False, message=f"Lease not found: {device_id}")

    except KeyboardInterrupt:
        raise
    except Exception as e:
        return DeviceReleaseResponse(success=False, message=str(e))


@router.post("/{device_id}/preempt", response_model=DevicePreemptResponse)
async def preempt_device(
    device_id: str,
    request: DevicePreemptRequest,
) -> DevicePreemptResponse:
    """Preempt a device from its current holder.

    Args:
        device_id: The device ID to preempt.
        request: Preemption request with reason.
        device_manager: Injected DeviceManager instance from daemon context.

    Returns:
        Response with success status and preempted client info.
    """
    from ..fastapi_app import get_daemon_context

    context = get_daemon_context()
    device_manager = context.device_manager

    if not request.reason:
        return DevicePreemptResponse(success=False, message="Reason is required for preemption")

    # Generate a client_id for the new holder
    import uuid

    client_id = f"http_client_{uuid.uuid4().hex[:8]}"

    try:
        success, preempted_client_id = device_manager.preempt_device(device_id=device_id, requesting_client_id=client_id, reason=request.reason)

        if success:
            return DevicePreemptResponse(success=True, preempted_client_id=preempted_client_id, message=f"Device preempted from {preempted_client_id}")

        return DevicePreemptResponse(success=False, message=f"Failed to preempt device: {device_id}")

    except KeyboardInterrupt:
        raise
    except Exception as e:
        return DevicePreemptResponse(success=False, message=str(e))
