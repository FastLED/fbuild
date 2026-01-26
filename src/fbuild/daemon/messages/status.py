"""
Daemon status and identity messages.

This module defines messages for querying daemon status, identity, and operational state.
"""

import time
from dataclasses import asdict, dataclass, field
from typing import Any

from fbuild.daemon.message_protocol import (
    EnumSerializationMixin,
    deserialize_dataclass,
    serialize_dataclass,
)
from fbuild.daemon.messages._base import DaemonState, OperationType


@dataclass
class DaemonStatus(EnumSerializationMixin):
    """Daemon → Client: Status update message.

    Attributes:
        state: Current daemon state
        message: Human-readable status message
        updated_at: Unix timestamp of last status update
        operation_in_progress: Whether an operation is actively running
        daemon_pid: Process ID of the daemon
        daemon_started_at: Unix timestamp when daemon started
        caller_pid: Process ID of client whose request is being processed
        caller_cwd: Working directory of client whose request is being processed
        request_id: ID of the request currently being processed
        request_started_at: Unix timestamp when current request started
        environment: Environment being processed
        project_dir: Project directory for current operation
        current_operation: Detailed description of current operation
        operation_type: Type of operation (deploy/monitor)
        output_lines: Recent output lines from the operation
        exit_code: Process exit code (None if still running)
        port: Serial port being used
        ports: Dictionary of active ports with their state information
        locks: Dictionary of lock state information (port_locks, project_locks)
    """

    state: DaemonState
    message: str
    updated_at: float
    operation_in_progress: bool = False
    daemon_pid: int | None = None
    daemon_started_at: float | None = None
    caller_pid: int | None = None
    caller_cwd: str | None = None
    request_id: str | None = None
    request_started_at: float | None = None
    environment: str | None = None
    project_dir: str | None = None
    current_operation: str | None = None
    operation_type: OperationType | None = None
    output_lines: list[str] = field(default_factory=list)
    exit_code: int | None = None
    port: str | None = None
    ports: dict[str, Any] = field(default_factory=dict)
    locks: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return serialize_dataclass(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "DaemonStatus":
        """Create DaemonStatus from dictionary."""
        return deserialize_dataclass(cls, data)

    def is_stale(self, timeout_seconds: float = 30.0) -> bool:
        """Check if status hasn't been updated recently."""
        return (time.time() - self.updated_at) > timeout_seconds

    def get_age_seconds(self) -> float:
        """Get age of this status update in seconds."""
        return time.time() - self.updated_at


@dataclass
class DaemonIdentity:
    """Identity information for a daemon instance.

    This is returned when clients query daemon identity and is used
    to distinguish between different daemon instances (e.g., dev vs prod).

    Attributes:
        name: Daemon name (e.g., "fbuild_daemon" or "fbuild_daemon_dev")
        version: Daemon version string
        started_at: Unix timestamp when daemon started
        spawned_by_pid: PID of client that originally started the daemon
        spawned_by_cwd: Working directory of client that started daemon
        is_dev: Whether this is a development mode daemon
        pid: Process ID of the daemon itself
    """

    name: str
    version: str
    started_at: float
    spawned_by_pid: int
    spawned_by_cwd: str
    is_dev: bool
    pid: int

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "DaemonIdentity":
        """Create DaemonIdentity from dictionary."""
        return cls(
            name=data["name"],
            version=data["version"],
            started_at=data["started_at"],
            spawned_by_pid=data["spawned_by_pid"],
            spawned_by_cwd=data["spawned_by_cwd"],
            is_dev=data["is_dev"],
            pid=data["pid"],
        )


@dataclass
class DaemonIdentityRequest:
    """Client -> Daemon: Request daemon identity information.

    Attributes:
        timestamp: Unix timestamp when request was created
    """

    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "DaemonIdentityRequest":
        """Create DaemonIdentityRequest from dictionary."""
        return cls(
            timestamp=data.get("timestamp", time.time()),
        )


@dataclass
class DaemonIdentityResponse:
    """Daemon -> Client: Response with daemon identity.

    Attributes:
        success: Whether the request succeeded
        message: Human-readable message
        identity: The daemon identity (if success)
        timestamp: Unix timestamp of response
    """

    success: bool
    message: str
    identity: DaemonIdentity | None = None
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        result = asdict(self)
        if self.identity:
            result["identity"] = self.identity.to_dict()
        return result

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "DaemonIdentityResponse":
        """Create DaemonIdentityResponse from dictionary."""
        identity = None
        if data.get("identity"):
            identity = DaemonIdentity.from_dict(data["identity"])
        return cls(
            success=data["success"],
            message=data["message"],
            identity=identity,
            timestamp=data.get("timestamp", time.time()),
        )
