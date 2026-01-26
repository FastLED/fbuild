"""
Serial session messages for managing serial port access.

This module defines messages for attaching, detaching, writing, and reading from serial sessions.
"""

import time
from dataclasses import asdict, dataclass, field
from typing import Any


@dataclass
class SerialAttachRequest:
    """Client → Daemon: Request to attach to a serial session.

    Attributes:
        client_id: Unique identifier for the client
        port: Serial port to attach to
        baud_rate: Baud rate for the connection
        as_reader: Whether to attach as a reader (True) or open the port (False)
        timestamp: Unix timestamp when request was created
    """

    client_id: str
    port: str
    baud_rate: int = 115200
    as_reader: bool = True
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "SerialAttachRequest":
        """Create SerialAttachRequest from dictionary."""
        return cls(
            client_id=data["client_id"],
            port=data["port"],
            baud_rate=data.get("baud_rate", 115200),
            as_reader=data.get("as_reader", True),
            timestamp=data.get("timestamp", time.time()),
        )


@dataclass
class SerialDetachRequest:
    """Client → Daemon: Request to detach from a serial session.

    Attributes:
        client_id: Unique identifier for the client
        port: Serial port to detach from
        close_port: Whether to close the port if this is the last reader
        timestamp: Unix timestamp when request was created
    """

    client_id: str
    port: str
    close_port: bool = False
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "SerialDetachRequest":
        """Create SerialDetachRequest from dictionary."""
        return cls(
            client_id=data["client_id"],
            port=data["port"],
            close_port=data.get("close_port", False),
            timestamp=data.get("timestamp", time.time()),
        )


@dataclass
class SerialWriteRequest:
    """Client → Daemon: Request to write data to a serial port.

    The client must have acquired writer access first.

    Attributes:
        client_id: Unique identifier for the client
        port: Serial port to write to
        data: Base64-encoded data to write
        acquire_writer: Whether to automatically acquire writer access if not held
        timestamp: Unix timestamp when request was created
    """

    client_id: str
    port: str
    data: str  # Base64-encoded bytes
    acquire_writer: bool = True
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "SerialWriteRequest":
        """Create SerialWriteRequest from dictionary."""
        return cls(
            client_id=data["client_id"],
            port=data["port"],
            data=data["data"],
            acquire_writer=data.get("acquire_writer", True),
            timestamp=data.get("timestamp", time.time()),
        )


@dataclass
class SerialBufferRequest:
    """Client → Daemon: Request to read buffered serial output.

    Attributes:
        client_id: Unique identifier for the client
        port: Serial port to read from
        max_lines: Maximum number of lines to return
        timestamp: Unix timestamp when request was created
    """

    client_id: str
    port: str
    max_lines: int = 100
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "SerialBufferRequest":
        """Create SerialBufferRequest from dictionary."""
        return cls(
            client_id=data["client_id"],
            port=data["port"],
            max_lines=data.get("max_lines", 100),
            timestamp=data.get("timestamp", time.time()),
        )


@dataclass
class SerialSessionResponse:
    """Daemon → Client: Response to serial session operations.

    Attributes:
        success: Whether the operation succeeded
        message: Human-readable status message
        is_open: Whether the port is currently open
        reader_count: Number of clients attached as readers
        has_writer: Whether a client has write access
        buffer_size: Number of lines in the output buffer
        lines: Output lines (for buffer requests)
        bytes_written: Number of bytes written (for write requests)
        timestamp: Unix timestamp of the response
    """

    success: bool
    message: str
    is_open: bool = False
    reader_count: int = 0
    has_writer: bool = False
    buffer_size: int = 0
    lines: list[str] = field(default_factory=list)
    bytes_written: int = 0
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "SerialSessionResponse":
        """Create SerialSessionResponse from dictionary."""
        return cls(
            success=data["success"],
            message=data["message"],
            is_open=data.get("is_open", False),
            reader_count=data.get("reader_count", 0),
            has_writer=data.get("has_writer", False),
            buffer_size=data.get("buffer_size", 0),
            lines=data.get("lines", []),
            bytes_written=data.get("bytes_written", 0),
            timestamp=data.get("timestamp", time.time()),
        )
