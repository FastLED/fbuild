"""
Lock management messages for resource synchronization.

This module defines messages for acquiring, releasing, and querying configuration locks.
"""

import time
from dataclasses import asdict, dataclass, field
from enum import Enum
from typing import Any


class LockType(Enum):
    """Type of lock to acquire."""

    EXCLUSIVE = "exclusive"
    SHARED_READ = "shared_read"


@dataclass
class LockAcquireRequest:
    """Client → Daemon: Request to acquire a configuration lock.

    Attributes:
        client_id: Unique identifier for the requesting client
        project_dir: Absolute path to project directory
        environment: Build environment name
        port: Serial port for the configuration
        lock_type: Type of lock to acquire (exclusive or shared_read)
        description: Human-readable description of the operation
        timeout: Maximum time in seconds to wait for the lock
        timestamp: Unix timestamp when request was created
    """

    client_id: str
    project_dir: str
    environment: str
    port: str
    lock_type: LockType
    description: str = ""
    timeout: float = 300.0  # 5 minutes default
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        result = asdict(self)
        result["lock_type"] = self.lock_type.value
        return result

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "LockAcquireRequest":
        """Create LockAcquireRequest from dictionary."""
        return cls(
            client_id=data["client_id"],
            project_dir=data["project_dir"],
            environment=data["environment"],
            port=data["port"],
            lock_type=LockType(data["lock_type"]),
            description=data.get("description", ""),
            timeout=data.get("timeout", 300.0),
            timestamp=data.get("timestamp", time.time()),
        )


@dataclass
class LockReleaseRequest:
    """Client → Daemon: Request to release a configuration lock.

    Attributes:
        client_id: Unique identifier for the client releasing the lock
        project_dir: Absolute path to project directory
        environment: Build environment name
        port: Serial port for the configuration
        timestamp: Unix timestamp when request was created
    """

    client_id: str
    project_dir: str
    environment: str
    port: str
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "LockReleaseRequest":
        """Create LockReleaseRequest from dictionary."""
        return cls(
            client_id=data["client_id"],
            project_dir=data["project_dir"],
            environment=data["environment"],
            port=data["port"],
            timestamp=data.get("timestamp", time.time()),
        )


@dataclass
class LockStatusRequest:
    """Client → Daemon: Request status of a configuration lock.

    Attributes:
        project_dir: Absolute path to project directory
        environment: Build environment name
        port: Serial port for the configuration
        timestamp: Unix timestamp when request was created
    """

    project_dir: str
    environment: str
    port: str
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "LockStatusRequest":
        """Create LockStatusRequest from dictionary."""
        return cls(
            project_dir=data["project_dir"],
            environment=data["environment"],
            port=data["port"],
            timestamp=data.get("timestamp", time.time()),
        )


@dataclass
class LockResponse:
    """Daemon → Client: Response to a lock request.

    Attributes:
        success: Whether the operation succeeded
        message: Human-readable status message
        lock_state: Current state of the lock ("unlocked", "locked_exclusive", "locked_shared_read")
        holder_count: Number of clients holding the lock
        waiting_count: Number of clients waiting for the lock
        timestamp: Unix timestamp of the response
    """

    success: bool
    message: str
    lock_state: str = "unlocked"
    holder_count: int = 0
    waiting_count: int = 0
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "LockResponse":
        """Create LockResponse from dictionary."""
        return cls(
            success=data["success"],
            message=data["message"],
            lock_state=data.get("lock_state", "unlocked"),
            holder_count=data.get("holder_count", 0),
            waiting_count=data.get("waiting_count", 0),
            timestamp=data.get("timestamp", time.time()),
        )
