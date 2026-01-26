"""
Device management messages for resource leasing and device discovery.

This module defines messages for acquiring exclusive or monitor access to devices,
listing devices, and querying device status.
"""

from __future__ import annotations

import time
from dataclasses import asdict, dataclass, field
from enum import Enum
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    pass


class DeviceLeaseType(Enum):
    """Type of device lease."""

    EXCLUSIVE = "exclusive"  # For deploy/flash/reset (single holder)
    MONITOR = "monitor"  # For read-only monitoring (multiple holders)


@dataclass
class DeviceLeaseRequest:
    """Client -> Daemon: Request to acquire a device lease.

    Used to acquire either exclusive access (for deploy/flash) or
    monitor access (for read-only serial monitoring).

    Attributes:
        device_id: The stable device ID to lease
        lease_type: Type of lease ("exclusive" or "monitor")
        description: Human-readable description of the operation
        allows_monitors: For exclusive leases, whether monitors are allowed (default True)
        timeout: Maximum time in seconds to wait for the lease
        timestamp: Unix timestamp when request was created
    """

    device_id: str
    lease_type: str  # "exclusive" or "monitor"
    description: str = ""
    allows_monitors: bool = True
    timeout: float = 300.0
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "DeviceLeaseRequest":
        """Create DeviceLeaseRequest from dictionary."""
        return cls(
            device_id=data["device_id"],
            lease_type=data["lease_type"],
            description=data.get("description", ""),
            allows_monitors=data.get("allows_monitors", True),
            timeout=data.get("timeout", 300.0),
            timestamp=data.get("timestamp", time.time()),
        )


@dataclass
class DeviceReleaseRequest:
    """Client -> Daemon: Request to release a device lease.

    Attributes:
        lease_id: The lease ID to release
        timestamp: Unix timestamp when request was created
    """

    lease_id: str
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "DeviceReleaseRequest":
        """Create DeviceReleaseRequest from dictionary."""
        return cls(
            lease_id=data["lease_id"],
            timestamp=data.get("timestamp", time.time()),
        )


@dataclass
class DevicePreemptRequest:
    """Client -> Daemon: Request to preempt a device's exclusive holder.

    Forcibly takes the exclusive lease from the current holder.
    The reason is REQUIRED and must not be empty.

    Attributes:
        device_id: The device to preempt
        reason: REQUIRED reason for preemption (must not be empty)
        timestamp: Unix timestamp when request was created
    """

    device_id: str
    reason: str  # REQUIRED - must not be empty
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "DevicePreemptRequest":
        """Create DevicePreemptRequest from dictionary."""
        return cls(
            device_id=data["device_id"],
            reason=data["reason"],
            timestamp=data.get("timestamp", time.time()),
        )


@dataclass
class DeviceListRequest:
    """Client -> Daemon: Request to list all devices.

    Attributes:
        include_disconnected: Whether to include disconnected devices
        refresh: Whether to refresh device discovery before listing
        timestamp: Unix timestamp when request was created
    """

    include_disconnected: bool = False
    refresh: bool = False
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "DeviceListRequest":
        """Create DeviceListRequest from dictionary."""
        return cls(
            include_disconnected=data.get("include_disconnected", False),
            refresh=data.get("refresh", False),
            timestamp=data.get("timestamp", time.time()),
        )


@dataclass
class DeviceStatusRequest:
    """Client -> Daemon: Request for detailed device status.

    Attributes:
        device_id: The device to get status for
        timestamp: Unix timestamp when request was created
    """

    device_id: str
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "DeviceStatusRequest":
        """Create DeviceStatusRequest from dictionary."""
        return cls(
            device_id=data["device_id"],
            timestamp=data.get("timestamp", time.time()),
        )


@dataclass
class DeviceLeaseResponse:
    """Daemon -> Client: Response to device lease operations.

    Attributes:
        success: Whether the operation succeeded
        message: Human-readable status message
        lease_id: The lease ID (if acquired)
        device_id: The device ID
        lease_type: Type of lease acquired
        allows_monitors: Whether monitors are allowed (for exclusive leases)
        preempted_client_id: Client ID that was preempted (for preempt operations)
        timestamp: Unix timestamp of the response
    """

    success: bool
    message: str
    lease_id: str | None = None
    device_id: str | None = None
    lease_type: str | None = None
    allows_monitors: bool = True
    preempted_client_id: str | None = None
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "DeviceLeaseResponse":
        """Create DeviceLeaseResponse from dictionary."""
        return cls(
            success=data["success"],
            message=data["message"],
            lease_id=data.get("lease_id"),
            device_id=data.get("device_id"),
            lease_type=data.get("lease_type"),
            allows_monitors=data.get("allows_monitors", True),
            preempted_client_id=data.get("preempted_client_id"),
            timestamp=data.get("timestamp", time.time()),
        )


@dataclass
class DeviceListResponse:
    """Daemon -> Client: Response to device list request.

    Attributes:
        success: Whether the operation succeeded
        message: Human-readable status message
        devices: List of device information dictionaries
        total_devices: Total number of devices
        connected_devices: Number of connected devices
        total_leases: Total number of active leases
        timestamp: Unix timestamp of the response
    """

    success: bool
    message: str
    devices: list[Any] = field(default_factory=list)  # list[DeviceState] - using Any to avoid circular import
    total_devices: int = 0
    connected_devices: int = 0
    total_leases: int = 0
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        result = asdict(self)
        # Manually serialize DeviceState objects
        result["devices"] = [device.to_dict() if hasattr(device, "to_dict") else device for device in self.devices]
        return result

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> DeviceListResponse:
        """Create DeviceListResponse from dictionary."""
        # Devices stay as dicts for now - full deserialization not needed
        return cls(
            success=data["success"],
            message=data["message"],
            devices=data.get("devices", []),
            total_devices=data.get("total_devices", 0),
            connected_devices=data.get("connected_devices", 0),
            total_leases=data.get("total_leases", 0),
            timestamp=data.get("timestamp", time.time()),
        )


@dataclass
class DeviceStatusResponse:
    """Daemon -> Client: Response to device status request.

    Attributes:
        success: Whether the operation succeeded
        message: Human-readable status message
        device_id: The device ID
        exists: Whether the device exists in the inventory
        is_connected: Whether the device is currently connected
        device_info: Full device information dictionary
        exclusive_lease: Current exclusive lease info (if any)
        monitor_leases: List of monitor lease info dictionaries
        monitor_count: Number of active monitor leases
        is_available_for_exclusive: Whether exclusive lease can be acquired
        timestamp: Unix timestamp of the response
    """

    success: bool
    message: str
    device_id: str = ""
    exists: bool = False
    is_connected: bool = False
    device_info: dict[str, Any] | None = None
    exclusive_lease: Any | None = None  # DeviceLease | None - using Any to avoid circular import
    monitor_leases: list[Any] = field(default_factory=list)  # list[DeviceLease] - using Any to avoid circular import
    monitor_count: int = 0
    is_available_for_exclusive: bool = False
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        result = asdict(self)
        # Manually serialize DeviceLease objects
        if self.exclusive_lease is not None and hasattr(self.exclusive_lease, "to_dict"):
            result["exclusive_lease"] = self.exclusive_lease.to_dict()
        result["monitor_leases"] = [lease.to_dict() if hasattr(lease, "to_dict") else lease for lease in self.monitor_leases]
        return result

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> DeviceStatusResponse:
        """Create DeviceStatusResponse from dictionary."""
        # Leases stay as dicts for now - full deserialization not needed
        return cls(
            success=data["success"],
            message=data["message"],
            device_id=data.get("device_id", ""),
            exists=data.get("exists", False),
            is_connected=data.get("is_connected", False),
            device_info=data.get("device_info"),
            exclusive_lease=data.get("exclusive_lease"),
            monitor_leases=data.get("monitor_leases", []),
            monitor_count=data.get("monitor_count", 0),
            is_available_for_exclusive=data.get("is_available_for_exclusive", False),
            timestamp=data.get("timestamp", time.time()),
        )


@dataclass
class DevicePreemptNotification:
    """Daemon -> Client: Notification that device was preempted.

    Sent to the client that was preempted from a device.

    Attributes:
        device_id: The device that was preempted
        preempted_by: Client ID of the requester
        reason: Reason for preemption (required)
        timestamp: Unix timestamp when preemption occurred
    """

    device_id: str
    preempted_by: str
    reason: str
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "DevicePreemptNotification":
        """Create DevicePreemptNotification from dictionary."""
        return cls(
            device_id=data["device_id"],
            preempted_by=data["preempted_by"],
            reason=data["reason"],
            timestamp=data.get("timestamp", time.time()),
        )
