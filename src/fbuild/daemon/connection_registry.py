"""
Server-side connection registry for tracking active daemon client connections.

This module provides the daemon-side tracking of all connected clients,
their state, and platform slot assignments. It is used by the daemon process
to manage concurrent client connections and coordinate resource allocation.

Key concepts:
- ConnectionState: Server-side state for a single client connection
- PlatformSlot: A platform-specific resource slot (e.g., esp32s3, esp32c6)
- ConnectionRegistry: Thread-safe registry managing all connections and slots
"""

import logging
import threading
import time
from dataclasses import asdict, dataclass
from typing import Any

logger = logging.getLogger(__name__)


@dataclass
class ConnectionState:
    """Server-side state for a single client connection.

    This dataclass tracks the state of a connected client as seen by the daemon.
    Each connection has a unique UUID and tracks project/environment context,
    heartbeat status, and any held platform slots.

    Attributes:
        connection_id: UUID for this connection
        project_dir: Client's project directory
        environment: Build environment (e.g., "esp32dev", "uno")
        platform: Target platform (e.g., "esp32s3", "esp32c6", "uno")
        connected_at: Connection timestamp (Unix time)
        last_heartbeat: Last heartbeat received (Unix time)
        firmware_uuid: UUID of current firmware (if deployed)
        slot_held: Platform slot currently held (if any)
        client_pid: Client process ID
        client_hostname: Client hostname
        client_version: Client version string
    """

    connection_id: str
    project_dir: str
    environment: str
    platform: str
    connected_at: float
    last_heartbeat: float
    firmware_uuid: str | None
    slot_held: str | None
    client_pid: int
    client_hostname: str
    client_version: str

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "ConnectionState":
        """Create ConnectionState from dictionary."""
        return cls(
            connection_id=data["connection_id"],
            project_dir=data["project_dir"],
            environment=data["environment"],
            platform=data["platform"],
            connected_at=data["connected_at"],
            last_heartbeat=data["last_heartbeat"],
            firmware_uuid=data.get("firmware_uuid"),
            slot_held=data.get("slot_held"),
            client_pid=data["client_pid"],
            client_hostname=data["client_hostname"],
            client_version=data["client_version"],
        )

    def is_stale(self, timeout_seconds: float = 30.0) -> bool:
        """Check if this connection has missed heartbeats.

        Args:
            timeout_seconds: Maximum allowed time since last heartbeat

        Returns:
            True if the connection is stale (heartbeat timeout exceeded)
        """
        return (time.time() - self.last_heartbeat) > timeout_seconds

    def get_age_seconds(self) -> float:
        """Get how long this connection has been active.

        Returns:
            Connection age in seconds
        """
        return time.time() - self.connected_at

    def get_idle_seconds(self) -> float:
        """Get how long since last heartbeat.

        Returns:
            Seconds since last heartbeat
        """
        return time.time() - self.last_heartbeat


@dataclass
class PlatformSlot:
    """A platform slot on the daemon (e.g., esp32s3, esp32c6, uno).

    Platform slots represent exclusive access to build/deploy for a specific
    platform. Only one connection can hold a slot at a time, ensuring that
    concurrent operations on the same platform are serialized.

    Attributes:
        platform: Platform identifier (e.g., "esp32s3", "esp32c6", "uno")
        current_connection_id: UUID of connection holding slot (None if free)
        current_firmware_uuid: UUID of deployed firmware (None if none)
        last_build_hash: Hash of last successful build (for incremental builds)
        locked_at: Timestamp when slot was acquired (None if free)
    """

    platform: str
    current_connection_id: str | None = None
    current_firmware_uuid: str | None = None
    last_build_hash: str | None = None
    locked_at: float | None = None

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "PlatformSlot":
        """Create PlatformSlot from dictionary."""
        return cls(
            platform=data["platform"],
            current_connection_id=data.get("current_connection_id"),
            current_firmware_uuid=data.get("current_firmware_uuid"),
            last_build_hash=data.get("last_build_hash"),
            locked_at=data.get("locked_at"),
        )

    def is_free(self) -> bool:
        """Check if this slot is available.

        Returns:
            True if no connection currently holds this slot
        """
        return self.current_connection_id is None

    def is_held_by(self, connection_id: str) -> bool:
        """Check if this slot is held by a specific connection.

        Args:
            connection_id: Connection UUID to check

        Returns:
            True if the specified connection holds this slot
        """
        return self.current_connection_id == connection_id

    def get_lock_duration(self) -> float | None:
        """Get how long this slot has been locked.

        Returns:
            Lock duration in seconds, or None if slot is free
        """
        if self.locked_at is None:
            return None
        return time.time() - self.locked_at


class ConnectionRegistry:
    """Server-side registry of all active client connections.

    This class manages the state of all connected clients and their platform
    slot assignments. It is thread-safe and uses a single lock for all
    mutations to ensure consistency.

    The registry supports:
    - Connection lifecycle (register, unregister, heartbeat)
    - Platform slot acquisition and release
    - Stale connection detection and cleanup
    - Firmware UUID tracking per connection

    Typical usage:
        registry = ConnectionRegistry(heartbeat_timeout=30.0)

        # Register a new connection
        state = registry.register_connection(
            connection_id="uuid-1234",
            project_dir="/path/to/project",
            environment="esp32dev",
            platform="esp32s3",
            client_pid=12345,
            client_hostname="localhost",
            client_version="1.2.11"
        )

        # Acquire platform slot
        if registry.acquire_slot("uuid-1234", "esp32s3"):
            # Do work...
            registry.release_slot("uuid-1234")

        # Cleanup on disconnect
        registry.unregister_connection("uuid-1234")
    """

    def __init__(self, heartbeat_timeout: float = 30.0) -> None:
        """Initialize the connection registry.

        Args:
            heartbeat_timeout: Maximum seconds allowed between heartbeats
                before a connection is considered stale. Default is 30 seconds.
        """
        self._lock = threading.Lock()
        self._heartbeat_timeout = heartbeat_timeout
        self._connections: dict[str, ConnectionState] = {}
        self._platform_slots: dict[str, PlatformSlot] = {}

    @property
    def connections(self) -> dict[str, ConnectionState]:
        """Get a copy of the connections dictionary.

        Returns:
            Copy of connection_id -> ConnectionState mapping
        """
        with self._lock:
            return dict(self._connections)

    @property
    def platform_slots(self) -> dict[str, PlatformSlot]:
        """Get a copy of the platform slots dictionary.

        Returns:
            Copy of platform -> PlatformSlot mapping
        """
        with self._lock:
            return dict(self._platform_slots)

    def register_connection(
        self,
        connection_id: str,
        project_dir: str,
        environment: str,
        platform: str,
        client_pid: int,
        client_hostname: str,
        client_version: str,
    ) -> ConnectionState:
        """Register a new client connection.

        Creates a new ConnectionState for the client and adds it to the registry.
        If a connection with the same ID already exists, it will be replaced.

        Args:
            connection_id: Unique UUID for this connection
            project_dir: Client's project directory
            environment: Build environment name
            platform: Target platform (e.g., "esp32s3")
            client_pid: Client process ID
            client_hostname: Client hostname
            client_version: Client version string

        Returns:
            The newly created ConnectionState
        """
        now = time.time()
        state = ConnectionState(
            connection_id=connection_id,
            project_dir=project_dir,
            environment=environment,
            platform=platform,
            connected_at=now,
            last_heartbeat=now,
            firmware_uuid=None,
            slot_held=None,
            client_pid=client_pid,
            client_hostname=client_hostname,
            client_version=client_version,
        )

        with self._lock:
            # If connection already exists, clean up any held resources first
            if connection_id in self._connections:
                logger.warning(f"Re-registering existing connection {connection_id}, cleaning up old state")
                self._release_slot_unlocked(connection_id)

            self._connections[connection_id] = state
            logger.info(f"Registered connection {connection_id} from {client_hostname} (pid={client_pid})")

        return state

    def unregister_connection(self, connection_id: str) -> bool:
        """Unregister a client connection.

        Removes the connection from the registry and releases any held slots.

        Args:
            connection_id: UUID of the connection to unregister

        Returns:
            True if the connection was found and removed, False if not found
        """
        with self._lock:
            if connection_id not in self._connections:
                logger.warning(f"Attempted to unregister unknown connection {connection_id}")
                return False

            # Release any held slot
            self._release_slot_unlocked(connection_id)

            # Remove the connection
            state = self._connections.pop(connection_id)
            logger.info(f"Unregistered connection {connection_id} (was connected for {state.get_age_seconds():.1f}s)")
            return True

    def update_heartbeat(self, connection_id: str) -> bool:
        """Update the heartbeat timestamp for a connection.

        Args:
            connection_id: UUID of the connection to update

        Returns:
            True if the connection was found and updated, False if not found
        """
        with self._lock:
            if connection_id not in self._connections:
                logger.debug(f"Heartbeat for unknown connection {connection_id}")
                return False

            self._connections[connection_id].last_heartbeat = time.time()
            return True

    def check_stale_connections(self) -> list[str]:
        """Check for connections that have missed heartbeats.

        Returns:
            List of connection IDs that are stale (heartbeat timeout exceeded)
        """
        stale_ids: list[str] = []
        now = time.time()

        with self._lock:
            for conn_id, state in self._connections.items():
                if (now - state.last_heartbeat) > self._heartbeat_timeout:
                    stale_ids.append(conn_id)

        return stale_ids

    def cleanup_stale_connections(self) -> int:
        """Clean up all stale connections.

        Removes connections that have exceeded the heartbeat timeout and
        releases any slots they were holding.

        Returns:
            Number of connections that were cleaned up
        """
        stale_ids = self.check_stale_connections()

        for conn_id in stale_ids:
            with self._lock:
                if conn_id in self._connections:
                    state = self._connections[conn_id]
                    idle_time = state.get_idle_seconds()
                    logger.warning(f"Cleaning up stale connection {conn_id} (idle for {idle_time:.1f}s)")
                    self._release_slot_unlocked(conn_id)
                    self._connections.pop(conn_id, None)

        if stale_ids:
            logger.info(f"Cleaned up {len(stale_ids)} stale connection(s)")

        return len(stale_ids)

    def acquire_slot(self, connection_id: str, platform: str) -> bool:
        """Acquire a platform slot for a connection.

        Attempts to acquire exclusive access to a platform slot. If the slot
        is already held by another connection, this will fail.

        Args:
            connection_id: UUID of the connection requesting the slot
            platform: Platform to acquire (e.g., "esp32s3")

        Returns:
            True if the slot was acquired, False if unavailable or error
        """
        with self._lock:
            # Verify connection exists
            if connection_id not in self._connections:
                logger.warning(f"Cannot acquire slot for unknown connection {connection_id}")
                return False

            state = self._connections[connection_id]

            # Check if connection already holds a slot
            if state.slot_held is not None:
                if state.slot_held == platform:
                    # Already holds this slot
                    logger.debug(f"Connection {connection_id} already holds slot {platform}")
                    return True
                else:
                    # Holds a different slot - must release first
                    logger.warning(f"Connection {connection_id} already holds slot {state.slot_held}, cannot acquire {platform}")
                    return False

            # Get or create the platform slot
            if platform not in self._platform_slots:
                self._platform_slots[platform] = PlatformSlot(platform=platform)

            slot = self._platform_slots[platform]

            # Check if slot is available
            if slot.current_connection_id is not None and slot.current_connection_id != connection_id:
                logger.debug(f"Slot {platform} is held by {slot.current_connection_id}, cannot acquire for {connection_id}")
                return False

            # Acquire the slot
            slot.current_connection_id = connection_id
            slot.locked_at = time.time()
            state.slot_held = platform

            logger.info(f"Connection {connection_id} acquired slot {platform}")
            return True

    def release_slot(self, connection_id: str) -> bool:
        """Release the platform slot held by a connection.

        Args:
            connection_id: UUID of the connection releasing its slot

        Returns:
            True if a slot was released, False if no slot was held
        """
        with self._lock:
            return self._release_slot_unlocked(connection_id)

    def _release_slot_unlocked(self, connection_id: str) -> bool:
        """Internal: Release slot without acquiring lock.

        Must be called while holding self._lock.

        Args:
            connection_id: UUID of the connection

        Returns:
            True if a slot was released
        """
        if connection_id not in self._connections:
            return False

        state = self._connections[connection_id]

        if state.slot_held is None:
            return False

        platform = state.slot_held
        if platform in self._platform_slots:
            slot = self._platform_slots[platform]
            if slot.current_connection_id == connection_id:
                slot.current_connection_id = None
                slot.locked_at = None
                logger.info(f"Connection {connection_id} released slot {platform}")

        state.slot_held = None
        return True

    def set_firmware_uuid(self, connection_id: str, firmware_uuid: str) -> bool:
        """Set the firmware UUID for a connection.

        Also updates the platform slot's firmware UUID if the connection
        holds a slot.

        Args:
            connection_id: UUID of the connection
            firmware_uuid: UUID of the deployed firmware

        Returns:
            True if the connection was found and updated, False if not found
        """
        with self._lock:
            if connection_id not in self._connections:
                logger.warning(f"Cannot set firmware UUID for unknown connection {connection_id}")
                return False

            state = self._connections[connection_id]
            state.firmware_uuid = firmware_uuid

            # Also update the platform slot if held
            if state.slot_held and state.slot_held in self._platform_slots:
                self._platform_slots[state.slot_held].current_firmware_uuid = firmware_uuid
                logger.debug(f"Updated firmware UUID for slot {state.slot_held}: {firmware_uuid}")

            return True

    def get_connection(self, connection_id: str) -> ConnectionState | None:
        """Get the state of a specific connection.

        Args:
            connection_id: UUID of the connection to retrieve

        Returns:
            ConnectionState if found, None otherwise
        """
        with self._lock:
            return self._connections.get(connection_id)

    def get_all_connections(self) -> list[ConnectionState]:
        """Get all active connections.

        Returns:
            List of all ConnectionState objects
        """
        with self._lock:
            return list(self._connections.values())

    def get_slot_status(self, platform: str) -> PlatformSlot | None:
        """Get the status of a specific platform slot.

        Args:
            platform: Platform to query (e.g., "esp32s3")

        Returns:
            PlatformSlot if it exists, None otherwise
        """
        with self._lock:
            return self._platform_slots.get(platform)

    def get_all_slots(self) -> dict[str, PlatformSlot]:
        """Get all platform slots.

        Returns:
            Copy of platform -> PlatformSlot mapping
        """
        with self._lock:
            return dict(self._platform_slots)

    def release_all_client_resources(self, connection_id: str) -> None:
        """Release all resources held by a client connection.

        This is called during graceful disconnect or stale connection cleanup.
        It releases any held slots and performs any necessary cleanup.

        Args:
            connection_id: UUID of the connection to clean up
        """
        with self._lock:
            if connection_id not in self._connections:
                return

            # Release slot if held
            self._release_slot_unlocked(connection_id)

            # Clear firmware UUID
            state = self._connections[connection_id]
            state.firmware_uuid = None

            logger.info(f"Released all resources for connection {connection_id}")

    def to_dict(self) -> dict[str, Any]:
        """Convert registry state to dictionary for status reporting.

        Returns:
            Dictionary containing:
            - connections: List of connection state dicts
            - platform_slots: Dict of platform -> slot state dicts
            - connection_count: Number of active connections
            - slot_count: Number of platform slots
            - heartbeat_timeout: Configured heartbeat timeout
        """
        with self._lock:
            return {
                "connections": [state.to_dict() for state in self._connections.values()],
                "platform_slots": {platform: slot.to_dict() for platform, slot in self._platform_slots.items()},
                "connection_count": len(self._connections),
                "slot_count": len(self._platform_slots),
                "heartbeat_timeout": self._heartbeat_timeout,
            }
