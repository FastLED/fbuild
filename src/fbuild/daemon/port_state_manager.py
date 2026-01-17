"""
Port State Manager - Tracks COM port state for visibility into daemon operations.

This module provides the PortStateManager class which tracks the state of all
COM ports in use by the daemon, providing visibility into which ports are in use,
by whom, and in what state.
"""

import logging
import threading
import time
from dataclasses import dataclass
from enum import Enum
from typing import Any


class PortState(Enum):
    """State of a serial port in use by the daemon."""

    AVAILABLE = "available"  # Port not in use
    UPLOADING = "uploading"  # Firmware being uploaded
    MONITORING = "monitoring"  # Serial monitor active
    RESERVED = "reserved"  # Reserved but not yet active


@dataclass
class PortInfo:
    """Information about a port currently in use.

    Attributes:
        port: Port identifier (e.g., "COM3", "/dev/ttyUSB0")
        state: Current state of the port
        client_pid: PID of client using the port
        project_dir: Project being deployed
        environment: Environment name
        operation_id: Request ID for the operation
        acquired_at: When port was acquired (Unix timestamp)
        last_activity: Last activity timestamp
    """

    port: str
    state: PortState = PortState.AVAILABLE
    client_pid: int | None = None
    project_dir: str | None = None
    environment: str | None = None
    operation_id: str | None = None
    acquired_at: float | None = None
    last_activity: float | None = None

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return {
            "port": self.port,
            "state": self.state.value,
            "client_pid": self.client_pid,
            "project_dir": self.project_dir,
            "environment": self.environment,
            "operation_id": self.operation_id,
            "acquired_at": self.acquired_at,
            "last_activity": self.last_activity,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "PortInfo":
        """Create PortInfo from dictionary."""
        state_str = data.get("state", "available")
        try:
            state = PortState(state_str)
        except ValueError:
            state = PortState.AVAILABLE

        return cls(
            port=data["port"],
            state=state,
            client_pid=data.get("client_pid"),
            project_dir=data.get("project_dir"),
            environment=data.get("environment"),
            operation_id=data.get("operation_id"),
            acquired_at=data.get("acquired_at"),
            last_activity=data.get("last_activity"),
        )


class PortStateManager:
    """Tracks state of all COM ports in use by the daemon.

    This class provides visibility into which ports are being used, by which
    clients, and in what state. It is thread-safe and can be accessed from
    multiple request handlers concurrently.

    Example:
        >>> manager = PortStateManager()
        >>> manager.acquire_port(
        ...     port="COM3",
        ...     state=PortState.UPLOADING,
        ...     client_pid=12345,
        ...     project_dir="/path/to/project",
        ...     environment="esp32c6",
        ...     operation_id="deploy_123"
        ... )
        >>> info = manager.get_port_info("COM3")
        >>> print(info.state)  # PortState.UPLOADING
        >>> manager.update_state("COM3", PortState.MONITORING)
        >>> manager.release_port("COM3")
    """

    def __init__(self) -> None:
        """Initialize the PortStateManager."""
        self._lock = threading.Lock()
        self._ports: dict[str, PortInfo] = {}

    def acquire_port(
        self,
        port: str,
        state: PortState,
        client_pid: int,
        project_dir: str,
        environment: str,
        operation_id: str,
    ) -> None:
        """Mark a port as in use.

        Args:
            port: Port identifier (e.g., "COM3", "/dev/ttyUSB0")
            state: Initial state for the port
            client_pid: PID of client using the port
            project_dir: Project being deployed
            environment: Environment name
            operation_id: Request ID for the operation
        """
        with self._lock:
            current_time = time.time()
            self._ports[port] = PortInfo(
                port=port,
                state=state,
                client_pid=client_pid,
                project_dir=project_dir,
                environment=environment,
                operation_id=operation_id,
                acquired_at=current_time,
                last_activity=current_time,
            )
            logging.debug(f"Port {port} acquired: state={state.value}, client_pid={client_pid}, operation_id={operation_id}")

    def update_state(self, port: str, state: PortState) -> None:
        """Update state of a port (e.g., UPLOADING -> MONITORING).

        Args:
            port: Port identifier
            state: New state for the port
        """
        with self._lock:
            if port in self._ports:
                old_state = self._ports[port].state
                self._ports[port].state = state
                self._ports[port].last_activity = time.time()
                logging.debug(f"Port {port} state updated: {old_state.value} -> {state.value}")
            else:
                logging.warning(f"Cannot update state for unknown port: {port}")

    def release_port(self, port: str) -> None:
        """Release a port back to available state.

        Args:
            port: Port identifier to release
        """
        with self._lock:
            if port in self._ports:
                info = self._ports[port]
                del self._ports[port]
                logging.debug(f"Port {port} released (was {info.state.value}, held for {time.time() - (info.acquired_at or 0):.1f}s)")
            else:
                logging.warning(f"Cannot release unknown port: {port}")

    def get_port_info(self, port: str) -> PortInfo | None:
        """Get info about a specific port.

        Args:
            port: Port identifier

        Returns:
            PortInfo for the port, or None if not tracked
        """
        with self._lock:
            info = self._ports.get(port)
            if info:
                # Return a copy to avoid race conditions
                return PortInfo(
                    port=info.port,
                    state=info.state,
                    client_pid=info.client_pid,
                    project_dir=info.project_dir,
                    environment=info.environment,
                    operation_id=info.operation_id,
                    acquired_at=info.acquired_at,
                    last_activity=info.last_activity,
                )
            return None

    def get_all_ports(self) -> dict[str, PortInfo]:
        """Get snapshot of all tracked ports.

        Returns:
            Dictionary mapping port names to PortInfo objects (copies)
        """
        with self._lock:
            return {
                port: PortInfo(
                    port=info.port,
                    state=info.state,
                    client_pid=info.client_pid,
                    project_dir=info.project_dir,
                    environment=info.environment,
                    operation_id=info.operation_id,
                    acquired_at=info.acquired_at,
                    last_activity=info.last_activity,
                )
                for port, info in self._ports.items()
            }

    def is_port_available(self, port: str) -> bool:
        """Check if port is available for use.

        Args:
            port: Port identifier

        Returns:
            True if port is not tracked (available), False if in use
        """
        with self._lock:
            return port not in self._ports

    def get_ports_summary(self) -> dict[str, dict[str, Any]]:
        """Get a summary of all port states for status reporting.

        Returns:
            Dictionary mapping port names to their info as dictionaries
        """
        with self._lock:
            return {port: info.to_dict() for port, info in self._ports.items()}

    def get_port_count(self) -> int:
        """Get the number of ports currently tracked.

        Returns:
            Number of ports in use
        """
        with self._lock:
            return len(self._ports)
