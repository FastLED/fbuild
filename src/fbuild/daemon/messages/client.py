"""
Client connection messages for daemon session management.

This module defines messages for client registration, heartbeats, and disconnection.
"""

import time
from dataclasses import asdict, dataclass, field
from typing import Any


@dataclass
class ClientConnectRequest:
    """Client → Daemon: Register a new client connection.

    Attributes:
        client_id: Unique identifier for the client (UUID)
        pid: Process ID of the client
        hostname: Hostname of the client machine
        version: Version of the client software
        timestamp: Unix timestamp when request was created
    """

    client_id: str
    pid: int
    hostname: str = ""
    version: str = ""
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "ClientConnectRequest":
        """Create ClientConnectRequest from dictionary."""
        return cls(
            client_id=data["client_id"],
            pid=data["pid"],
            hostname=data.get("hostname", ""),
            version=data.get("version", ""),
            timestamp=data.get("timestamp", time.time()),
        )


@dataclass
class ClientHeartbeatRequest:
    """Client → Daemon: Periodic heartbeat to indicate client is alive.

    Attributes:
        client_id: Unique identifier for the client
        timestamp: Unix timestamp when heartbeat was sent
    """

    client_id: str
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "ClientHeartbeatRequest":
        """Create ClientHeartbeatRequest from dictionary."""
        return cls(
            client_id=data["client_id"],
            timestamp=data.get("timestamp", time.time()),
        )


@dataclass
class ClientDisconnectRequest:
    """Client → Daemon: Graceful disconnect notification.

    Attributes:
        client_id: Unique identifier for the client
        reason: Optional reason for disconnection
        timestamp: Unix timestamp when disconnect was initiated
    """

    client_id: str
    reason: str = ""
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "ClientDisconnectRequest":
        """Create ClientDisconnectRequest from dictionary."""
        return cls(
            client_id=data["client_id"],
            reason=data.get("reason", ""),
            timestamp=data.get("timestamp", time.time()),
        )


@dataclass
class ClientResponse:
    """Daemon → Client: Response to client connection operations.

    Attributes:
        success: Whether the operation succeeded
        message: Human-readable status message
        client_id: Client ID (may be assigned by daemon)
        is_registered: Whether the client is currently registered
        total_clients: Total number of connected clients
        timestamp: Unix timestamp of the response
    """

    success: bool
    message: str
    client_id: str = ""
    is_registered: bool = False
    total_clients: int = 0
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "ClientResponse":
        """Create ClientResponse from dictionary."""
        return cls(
            success=data["success"],
            message=data["message"],
            client_id=data.get("client_id", ""),
            is_registered=data.get("is_registered", False),
            total_clients=data.get("total_clients", 0),
            timestamp=data.get("timestamp", time.time()),
        )
