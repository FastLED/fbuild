"""
Device Manager - Manages device inventory and leases for the fbuild daemon.

This module provides the DeviceManager class which handles:

- Device inventory tracking (discovered and tracked devices)
- Exclusive leases for deploy/flash/reset operations (single holder)
- Monitor leases for read-only access (multiple holders allowed)
- Automatic lease release on client disconnect
- Device preemption with mandatory reason
- Thread-safe operations using memory-based locks

The DeviceManager follows the same patterns as ConfigurationLockManager,
using threading.Lock for thread safety as per the project's locking strategy.
"""

import _thread
import logging
import threading
import time
import uuid
from dataclasses import dataclass, field
from enum import Enum
from typing import Any

from fbuild.daemon.device_discovery import DeviceInfo, discover_devices


class LeaseType(Enum):
    """Type of device lease."""

    EXCLUSIVE = "exclusive"  # For deploy/flash/reset operations
    MONITOR = "monitor"  # For read-only monitoring (shared)


@dataclass
class DeviceLease:
    """Represents an active lease on a device.

    Attributes:
        device_id: The stable device ID this lease is for
        lease_id: Unique identifier for this lease (UUID)
        client_id: The client holding this lease
        lease_type: Type of lease (exclusive or monitor)
        description: Human-readable description of the operation
        acquired_at: Unix timestamp when lease was acquired
        allows_monitors: If exclusive, whether monitors are allowed (default True)
    """

    device_id: str
    lease_id: str
    client_id: str
    lease_type: LeaseType
    description: str
    acquired_at: float = field(default_factory=time.time)
    allows_monitors: bool = True

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return {
            "device_id": self.device_id,
            "lease_id": self.lease_id,
            "client_id": self.client_id,
            "lease_type": self.lease_type.value,
            "description": self.description,
            "acquired_at": self.acquired_at,
            "allows_monitors": self.allows_monitors,
            "hold_duration": time.time() - self.acquired_at,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "DeviceLease":
        """Create DeviceLease from dictionary."""
        return cls(
            device_id=data["device_id"],
            lease_id=data["lease_id"],
            client_id=data["client_id"],
            lease_type=LeaseType(data["lease_type"]),
            description=data.get("description", ""),
            acquired_at=data.get("acquired_at", time.time()),
            allows_monitors=data.get("allows_monitors", True),
        )


@dataclass
class DeviceState:
    """Tracks the state of a device including its leases.

    Attributes:
        device_id: The stable device ID
        device_info: Full device information
        exclusive_lease: Current exclusive lease holder (None if none)
        monitor_leases: Dictionary of lease_id -> DeviceLease for monitors
        last_seen_at: Unix timestamp when device was last seen
        is_connected: Whether the device is currently connected
    """

    device_id: str
    device_info: DeviceInfo
    exclusive_lease: DeviceLease | None = None
    monitor_leases: dict[str, DeviceLease] = field(default_factory=dict)
    last_seen_at: float = field(default_factory=time.time)
    is_connected: bool = True

    def is_available_for_exclusive(self) -> bool:
        """Check if device is available for exclusive lease."""
        return self.exclusive_lease is None

    def has_any_lease(self) -> bool:
        """Check if device has any active leases."""
        return self.exclusive_lease is not None or len(self.monitor_leases) > 0

    def lease_count(self) -> int:
        """Get total number of leases on this device."""
        count = len(self.monitor_leases)
        if self.exclusive_lease:
            count += 1
        return count

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return {
            "device_id": self.device_id,
            "device_info": self.device_info.to_dict(),
            "exclusive_lease": self.exclusive_lease.to_dict() if self.exclusive_lease else None,
            "monitor_leases": {lease_id: lease.to_dict() for lease_id, lease in self.monitor_leases.items()},
            "monitor_count": len(self.monitor_leases),
            "last_seen_at": self.last_seen_at,
            "is_connected": self.is_connected,
            "is_available_for_exclusive": self.is_available_for_exclusive(),
            "has_any_lease": self.has_any_lease(),
            "lease_count": self.lease_count(),
        }


class DeviceManagerError(RuntimeError):
    """Error raised when a device manager operation fails."""

    def __init__(
        self,
        message: str,
        device_id: str | None = None,
        lease_id: str | None = None,
    ):
        self.device_id = device_id
        self.lease_id = lease_id
        super().__init__(message)


class DeviceManager:
    """Manages device inventory and leases.

    This class provides thread-safe device lease management following
    the same patterns as ConfigurationLockManager:
    - Memory-based locks only (threading.Lock)
    - No file-based locking
    - Idempotent release operations
    - Auto-release on client disconnect

    Thread Safety:
        All public methods are thread-safe using an internal master lock.

    Example:
        >>> manager = DeviceManager()
        >>> manager.refresh_devices()  # Discover connected devices
        >>>
        >>> # Acquire exclusive lease for deploy
        >>> lease = manager.acquire_exclusive(
        ...     device_id="usb-ABC123",
        ...     client_id="client-001",
        ...     description="Deploying firmware"
        ... )
        >>> if lease:
        ...     try:
        ...         do_deploy()
        ...     finally:
        ...         manager.release_lease(lease.lease_id, "client-001")
        >>>
        >>> # Acquire monitor lease
        >>> monitor_lease = manager.acquire_monitor(
        ...     device_id="usb-ABC123",
        ...     client_id="client-002",
        ...     description="Monitoring serial output"
        ... )
    """

    def __init__(self) -> None:
        """Initialize the DeviceManager."""
        self._master_lock = threading.Lock()
        self._devices: dict[str, DeviceState] = {}  # device_id -> DeviceState
        self._client_leases: dict[str, set[str]] = {}  # client_id -> set of lease_ids
        self._lease_to_device: dict[str, str] = {}  # lease_id -> device_id

        logging.info("DeviceManager initialized")

    def _generate_lease_id(self) -> str:
        """Generate a unique lease ID."""
        return str(uuid.uuid4())

    def _track_client_lease(self, client_id: str, lease_id: str, device_id: str) -> None:
        """Track that a client holds a lease.

        Must be called with _master_lock held.
        """
        if client_id not in self._client_leases:
            self._client_leases[client_id] = set()
        self._client_leases[client_id].add(lease_id)
        self._lease_to_device[lease_id] = device_id

    def _untrack_client_lease(self, client_id: str, lease_id: str) -> None:
        """Stop tracking that a client holds a lease.

        Must be called with _master_lock held.
        """
        if client_id in self._client_leases:
            self._client_leases[client_id].discard(lease_id)
            if not self._client_leases[client_id]:
                del self._client_leases[client_id]
        self._lease_to_device.pop(lease_id, None)

    def refresh_devices(self) -> list[DeviceInfo]:
        """Refresh device inventory from hardware.

        Discovers all connected devices and updates the internal inventory.
        Existing leases are preserved for devices that remain connected.
        Devices that are no longer connected are marked as disconnected.

        Returns:
            List of currently connected DeviceInfo objects.
        """
        try:
            discovered = discover_devices()
        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception as e:
            logging.error(f"Error discovering devices: {e}")
            return []

        current_time = time.time()
        discovered_ids = set()

        with self._master_lock:
            # Update or add discovered devices
            for device_info in discovered:
                device_id = device_info.device_id
                discovered_ids.add(device_id)

                if device_id in self._devices:
                    # Update existing device
                    state = self._devices[device_id]
                    state.device_info = device_info
                    state.last_seen_at = current_time
                    state.is_connected = True
                    logging.debug(f"Updated device: {device_id}")
                else:
                    # Add new device
                    state = DeviceState(
                        device_id=device_id,
                        device_info=device_info,
                        last_seen_at=current_time,
                        is_connected=True,
                    )
                    self._devices[device_id] = state
                    logging.info(f"Discovered new device: {device_id} ({device_info.port})")

            # Mark devices that weren't discovered as disconnected
            for device_id, state in self._devices.items():
                if device_id not in discovered_ids and state.is_connected:
                    state.is_connected = False
                    logging.info(f"Device disconnected: {device_id}")

        logging.info(f"Device refresh complete: {len(discovered)} connected device(s)")
        return discovered

    def get_device(self, device_id: str) -> DeviceState | None:
        """Get device state by ID.

        Args:
            device_id: The stable device ID

        Returns:
            DeviceState if found, None otherwise
        """
        with self._master_lock:
            return self._devices.get(device_id)

    def get_device_by_port(self, port: str) -> DeviceState | None:
        """Get device state by port name.

        Args:
            port: The serial port (e.g., "COM3")

        Returns:
            DeviceState if found, None otherwise
        """
        with self._master_lock:
            for state in self._devices.values():
                if state.device_info.matches_port(port):
                    return state
        return None

    def get_all_devices(self) -> dict[str, DeviceState]:
        """Get all tracked devices.

        Returns:
            Dictionary of device_id -> DeviceState
        """
        with self._master_lock:
            # Return a shallow copy to avoid external modification
            return dict(self._devices)

    def get_connected_devices(self) -> list[DeviceState]:
        """Get all currently connected devices.

        Returns:
            List of DeviceState for connected devices
        """
        with self._master_lock:
            return [s for s in self._devices.values() if s.is_connected]

    def acquire_exclusive(
        self,
        device_id: str,
        client_id: str,
        description: str = "",
        allows_monitors: bool = True,
        timeout: float = 300.0,
    ) -> DeviceLease | None:
        """Acquire exclusive lease for deploy/flash/reset.

        An exclusive lease can only be held by one client at a time.
        Monitor leases may be allowed depending on allows_monitors.

        Args:
            device_id: The stable device ID to lease
            client_id: UUID string identifying the client
            description: Human-readable description of the operation
            allows_monitors: Whether to allow monitor leases while holding exclusive
            timeout: Reserved for future queuing support

        Returns:
            DeviceLease if acquired, None if not available
        """
        with self._master_lock:
            # Check if device exists
            state = self._devices.get(device_id)
            if state is None:
                logging.debug(f"Device {device_id} not found for exclusive lease")
                return None

            # Check if device is connected
            if not state.is_connected:
                logging.debug(f"Device {device_id} is disconnected, cannot acquire exclusive lease")
                return None

            # Check if already held by this client (reentrant case)
            if state.exclusive_lease and state.exclusive_lease.client_id == client_id:
                logging.debug(f"Client {client_id} already holds exclusive lease for {device_id}")
                return state.exclusive_lease

            # Check if available for exclusive
            if not state.is_available_for_exclusive():
                holder = state.exclusive_lease
                logging.debug(f"Exclusive lease not available for {device_id}, held by {holder.client_id if holder else 'unknown'}")
                return None

            # Acquire exclusive lease
            lease_id = self._generate_lease_id()
            lease = DeviceLease(
                device_id=device_id,
                lease_id=lease_id,
                client_id=client_id,
                lease_type=LeaseType.EXCLUSIVE,
                description=description,
                allows_monitors=allows_monitors,
            )
            state.exclusive_lease = lease
            self._track_client_lease(client_id, lease_id, device_id)

            logging.info(f"Exclusive lease acquired for {device_id} by {client_id} (lease={lease_id})")
            return lease

    def acquire_monitor(
        self,
        device_id: str,
        client_id: str,
        description: str = "",
    ) -> DeviceLease | None:
        """Acquire monitor lease for read-only access.

        Multiple clients can hold monitor leases simultaneously.
        Monitor leases can be acquired when:
        - No exclusive lease is held, OR
        - The exclusive lease holder allows monitors

        Args:
            device_id: The stable device ID to lease
            client_id: UUID string identifying the client
            description: Human-readable description of the operation

        Returns:
            DeviceLease if acquired, None if not available
        """
        with self._master_lock:
            # Check if device exists
            state = self._devices.get(device_id)
            if state is None:
                logging.debug(f"Device {device_id} not found for monitor lease")
                return None

            # Check if device is connected
            if not state.is_connected:
                logging.debug(f"Device {device_id} is disconnected, cannot acquire monitor lease")
                return None

            # Check if client already has a monitor lease
            for lease in state.monitor_leases.values():
                if lease.client_id == client_id:
                    logging.debug(f"Client {client_id} already has monitor lease for {device_id}")
                    return lease

            # Check if monitors are allowed
            if state.exclusive_lease and not state.exclusive_lease.allows_monitors:
                logging.debug(f"Monitor lease not allowed for {device_id}, exclusive holder {state.exclusive_lease.client_id} disallows monitors")
                return None

            # Acquire monitor lease
            lease_id = self._generate_lease_id()
            lease = DeviceLease(
                device_id=device_id,
                lease_id=lease_id,
                client_id=client_id,
                lease_type=LeaseType.MONITOR,
                description=description,
            )
            state.monitor_leases[lease_id] = lease
            self._track_client_lease(client_id, lease_id, device_id)

            logging.info(f"Monitor lease acquired for {device_id} by {client_id} (lease={lease_id}, total monitors={len(state.monitor_leases)})")
            return lease

    def release_lease(self, lease_id: str, client_id: str) -> bool:
        """Release a specific lease.

        This operation is idempotent - releasing a non-existent lease
        returns False but does not raise an error.

        Args:
            lease_id: The lease ID to release
            client_id: The client releasing the lease (must match lease holder)

        Returns:
            True if a lease was released, False otherwise
        """
        with self._master_lock:
            # Find the device for this lease
            device_id = self._lease_to_device.get(lease_id)
            if device_id is None:
                logging.debug(f"Lease {lease_id} not found for release")
                return False

            state = self._devices.get(device_id)
            if state is None:
                logging.debug(f"Device {device_id} not found for lease release")
                return False

            # Check if this is the exclusive lease
            if state.exclusive_lease and state.exclusive_lease.lease_id == lease_id:
                if state.exclusive_lease.client_id != client_id:
                    logging.warning(f"Client {client_id} tried to release exclusive lease held by {state.exclusive_lease.client_id}")
                    return False

                state.exclusive_lease = None
                self._untrack_client_lease(client_id, lease_id)
                logging.info(f"Exclusive lease {lease_id} released for {device_id} by {client_id}")
                return True

            # Check if this is a monitor lease
            if lease_id in state.monitor_leases:
                lease = state.monitor_leases[lease_id]
                if lease.client_id != client_id:
                    logging.warning(f"Client {client_id} tried to release monitor lease held by {lease.client_id}")
                    return False

                del state.monitor_leases[lease_id]
                self._untrack_client_lease(client_id, lease_id)
                logging.info(f"Monitor lease {lease_id} released for {device_id} by {client_id} (remaining monitors={len(state.monitor_leases)})")
                return True

            logging.debug(f"Lease {lease_id} not found on device {device_id}")
            return False

    def release_all_client_leases(self, client_id: str) -> int:
        """Release all leases held by a client.

        This should be called when a client disconnects to ensure
        all its leases are properly released.

        Args:
            client_id: UUID string identifying the client

        Returns:
            Number of leases released
        """
        released_count = 0

        with self._master_lock:
            # Get copy of lease IDs since we'll be modifying the set
            lease_ids = list(self._client_leases.get(client_id, set()))

        # Release each lease (release_lease will acquire _master_lock internally)
        for lease_id in lease_ids:
            if self.release_lease(lease_id, client_id):
                released_count += 1

        if released_count > 0:
            logging.info(f"Released {released_count} lease(s) for disconnected client {client_id}")

        return released_count

    def preempt_device(
        self,
        device_id: str,
        requesting_client_id: str,
        reason: str,
    ) -> tuple[bool, str | None]:
        """Preempt current exclusive holder.

        This forcibly takes the exclusive lease from the current holder
        and transfers it to the requesting client. The reason is mandatory
        and will be logged.

        Args:
            device_id: The device to preempt
            requesting_client_id: The client requesting preemption
            reason: REQUIRED reason for preemption (must not be empty)

        Returns:
            Tuple of (success, preempted_client_id or None)

        Raises:
            DeviceManagerError: If reason is empty
        """
        if not reason or not reason.strip():
            raise DeviceManagerError(
                "Preemption reason is required and must not be empty",
                device_id=device_id,
            )

        with self._master_lock:
            state = self._devices.get(device_id)
            if state is None:
                logging.debug(f"Device {device_id} not found for preemption")
                return (False, None)

            if state.exclusive_lease is None:
                logging.debug(f"No exclusive lease to preempt on device {device_id}")
                return (False, None)

            preempted_client_id = state.exclusive_lease.client_id
            preempted_lease_id = state.exclusive_lease.lease_id

            # Log the preemption with full details
            logging.warning(f"PREEMPTION: Device {device_id} taken from client {preempted_client_id} by client {requesting_client_id}. Reason: {reason}")

            # Release the current lease
            state.exclusive_lease = None
            self._untrack_client_lease(preempted_client_id, preempted_lease_id)

            # Acquire new exclusive lease for the requesting client
            lease_id = self._generate_lease_id()
            lease = DeviceLease(
                device_id=device_id,
                lease_id=lease_id,
                client_id=requesting_client_id,
                lease_type=LeaseType.EXCLUSIVE,
                description=f"Preempted from {preempted_client_id}: {reason}",
            )
            state.exclusive_lease = lease
            self._track_client_lease(requesting_client_id, lease_id, device_id)

            logging.info(f"Preemption complete: {requesting_client_id} now holds exclusive lease for {device_id} (lease={lease_id})")

            return (True, preempted_client_id)

    def get_device_status(self, device_id: str) -> dict[str, Any]:
        """Get detailed status of a device.

        Args:
            device_id: The device to get status for

        Returns:
            Dictionary with device state, leases, and availability
        """
        with self._master_lock:
            state = self._devices.get(device_id)

            if state is None:
                return {
                    "device_id": device_id,
                    "exists": False,
                    "is_connected": False,
                    "exclusive_lease": None,
                    "monitor_leases": {},
                    "monitor_count": 0,
                    "is_available_for_exclusive": False,
                    "has_any_lease": False,
                }

            return {
                "exists": True,
                **state.to_dict(),
            }

    def get_all_leases(self) -> dict[str, Any]:
        """Get status of all device leases.

        Returns:
            Dictionary with lease information for all devices
        """
        with self._master_lock:
            result: dict[str, Any] = {
                "devices": {},
                "total_devices": len(self._devices),
                "connected_devices": sum(1 for s in self._devices.values() if s.is_connected),
                "total_leases": sum(s.lease_count() for s in self._devices.values()),
                "total_clients": len(self._client_leases),
                "summary": {
                    "exclusive_leases": sum(1 for s in self._devices.values() if s.exclusive_lease is not None),
                    "monitor_leases": sum(len(s.monitor_leases) for s in self._devices.values()),
                },
            }

            for device_id, state in self._devices.items():
                result["devices"][device_id] = state.to_dict()

            return result

    def get_client_leases(self, client_id: str) -> list[dict[str, Any]]:
        """Get all leases held by a specific client.

        Args:
            client_id: UUID string identifying the client

        Returns:
            List of lease information dictionaries
        """
        with self._master_lock:
            lease_ids = self._client_leases.get(client_id, set())
            result: list[dict[str, Any]] = []

            for lease_id in lease_ids:
                device_id = self._lease_to_device.get(lease_id)
                if device_id is None:
                    continue

                state = self._devices.get(device_id)
                if state is None:
                    continue

                # Check exclusive lease
                if state.exclusive_lease and state.exclusive_lease.lease_id == lease_id:
                    result.append(state.exclusive_lease.to_dict())
                # Check monitor leases
                elif lease_id in state.monitor_leases:
                    result.append(state.monitor_leases[lease_id].to_dict())

            return result

    def is_available_for_exclusive(self, device_id: str) -> bool:
        """Check if exclusive lease can be immediately acquired.

        Args:
            device_id: The device to check

        Returns:
            True if exclusive lease is immediately available
        """
        with self._master_lock:
            state = self._devices.get(device_id)
            if state is None:
                return False
            return state.is_available_for_exclusive() and state.is_connected

    def cleanup_stale_devices(self, older_than: float = 3600.0) -> int:
        """Clean up devices that haven't been seen recently.

        This removes device entries that are:
        - Not currently connected
        - Have no active leases
        - Haven't been seen in the specified time period

        Args:
            older_than: Time in seconds. Devices not seen longer than this are removed.

        Returns:
            Number of devices removed
        """
        current_time = time.time()
        removed_count = 0

        with self._master_lock:
            devices_to_remove = []

            for device_id, state in self._devices.items():
                if state.is_connected:
                    continue  # Don't remove connected devices

                if state.has_any_lease():
                    continue  # Don't remove devices with active leases

                if current_time - state.last_seen_at > older_than:
                    devices_to_remove.append(device_id)

            for device_id in devices_to_remove:
                del self._devices[device_id]
                removed_count += 1
                logging.debug(f"Cleaned up stale device: {device_id}")

        if removed_count > 0:
            logging.info(f"Cleaned up {removed_count} stale device(s)")

        return removed_count

    def clear_all_leases(self) -> int:
        """Clear all leases (use with extreme caution - only for daemon restart).

        This releases all leases and clears all internal tracking state.
        Should only be used during daemon shutdown/restart.

        Returns:
            Number of leases cleared
        """
        with self._master_lock:
            count = 0

            for state in self._devices.values():
                if state.exclusive_lease:
                    count += 1
                    state.exclusive_lease = None
                count += len(state.monitor_leases)
                state.monitor_leases.clear()

            self._client_leases.clear()
            self._lease_to_device.clear()

            if count > 0:
                logging.info(f"Cleared all {count} device lease(s)")

            return count

    def register_qemu_device(
        self,
        instance_id: str,
        description: str = "QEMU Virtual Device",
    ) -> DeviceState:
        """Register a QEMU virtual device.

        QEMU devices don't have physical serial ports but still need
        to be tracked for lease management.

        Args:
            instance_id: Unique identifier for the QEMU instance
            description: Human-readable description

        Returns:
            DeviceState for the registered QEMU device
        """
        from fbuild.daemon.device_discovery import create_qemu_device

        device_info = create_qemu_device(instance_id, description)

        with self._master_lock:
            if device_info.device_id in self._devices:
                state = self._devices[device_info.device_id]
                state.device_info = device_info
                state.last_seen_at = time.time()
                state.is_connected = True
                logging.debug(f"Updated QEMU device: {device_info.device_id}")
            else:
                state = DeviceState(
                    device_id=device_info.device_id,
                    device_info=device_info,
                    last_seen_at=time.time(),
                    is_connected=True,
                )
                self._devices[device_info.device_id] = state
                logging.info(f"Registered QEMU device: {device_info.device_id}")

            return state

    def unregister_qemu_device(self, instance_id: str) -> bool:
        """Unregister a QEMU virtual device.

        Args:
            instance_id: The QEMU instance ID to unregister

        Returns:
            True if device was unregistered, False if not found
        """
        device_id = f"qemu-{instance_id}"

        with self._master_lock:
            if device_id not in self._devices:
                return False

            state = self._devices[device_id]

            # Release any leases first
            if state.exclusive_lease:
                self._untrack_client_lease(
                    state.exclusive_lease.client_id,
                    state.exclusive_lease.lease_id,
                )
            for lease in list(state.monitor_leases.values()):
                self._untrack_client_lease(lease.client_id, lease.lease_id)

            del self._devices[device_id]
            logging.info(f"Unregistered QEMU device: {device_id}")
            return True
