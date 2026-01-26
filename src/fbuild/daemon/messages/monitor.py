"""
Serial monitor API messages for fbuild.api.SerialMonitor.

This module defines messages for the serial monitor API, which provides
read-only monitoring of serial ports via the daemon.
"""

import time
import uuid
from dataclasses import dataclass, field
from typing import Any

from fbuild.daemon.message_protocol import deserialize_dataclass, serialize_dataclass


@dataclass
class SerialMonitorAttachRequest:
    """Client → Daemon: Request to attach as reader to serial session.

    Used by fbuild.api.SerialMonitor to attach to daemon-managed serial port.

    Attributes:
        request_id: Unique identifier for this request (UUID) - for response correlation
        client_id: Unique identifier for the client (UUID)
        port: Serial port to attach to (e.g., "COM13", "/dev/ttyUSB0")
        baud_rate: Baud rate for the connection (default: 115200)
        open_if_needed: Whether to open the port if not already open (default: True)
        timestamp: Unix timestamp when request was created
    """

    request_id: str = field(default_factory=lambda: str(uuid.uuid4()))
    client_id: str = ""
    port: str = ""
    baud_rate: int = 115200
    open_if_needed: bool = True
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return serialize_dataclass(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "SerialMonitorAttachRequest":
        """Create SerialMonitorAttachRequest from dictionary."""
        return deserialize_dataclass(cls, data)


@dataclass
class SerialMonitorDetachRequest:
    """Client → Daemon: Request to detach from serial session.

    Attributes:
        request_id: Unique identifier for this request (UUID) - for response correlation
        client_id: Unique identifier for the client
        port: Serial port to detach from
        timestamp: Unix timestamp when request was created
    """

    request_id: str = field(default_factory=lambda: str(uuid.uuid4()))
    client_id: str = ""
    port: str = ""
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return serialize_dataclass(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "SerialMonitorDetachRequest":
        """Create SerialMonitorDetachRequest from dictionary."""
        return deserialize_dataclass(cls, data)


@dataclass
class SerialMonitorPollRequest:
    """Client → Daemon: Request to poll for new output lines.

    Uses incremental polling where client tracks last_index to avoid
    re-reading old lines.

    Attributes:
        request_id: Unique identifier for this request (UUID) - for response correlation
        client_id: Unique identifier for the client
        port: Serial port to poll
        last_index: Last line index received (0 for initial poll)
        max_lines: Maximum number of lines to return per poll (default: 100)
        timestamp: Unix timestamp when request was created
    """

    request_id: str = field(default_factory=lambda: str(uuid.uuid4()))
    client_id: str = ""
    port: str = ""
    last_index: int = 0
    max_lines: int = 100
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return serialize_dataclass(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "SerialMonitorPollRequest":
        """Create SerialMonitorPollRequest from dictionary."""
        return deserialize_dataclass(cls, data)


@dataclass
class SerialMonitorResponse:
    """Daemon → Client: Response to serial monitor API operations.

    Used for attach, detach, poll, and write responses.

    Attributes:
        request_id: Unique identifier for the original request - for correlation
        success: Whether the operation succeeded
        message: Human-readable status message
        lines: New output lines (for poll responses)
        current_index: Current buffer position for incremental polling
        is_preempted: Whether deploy has preempted monitoring
        preempted_by: Client ID that preempted (if is_preempted=True)
        bytes_written: Number of bytes written (for write responses)
        timestamp: Unix timestamp of the response
    """

    request_id: str = ""
    success: bool = False
    message: str = ""
    lines: list[str] = field(default_factory=list)
    current_index: int = 0
    is_preempted: bool = False
    preempted_by: str | None = None
    bytes_written: int = 0
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return serialize_dataclass(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "SerialMonitorResponse":
        """Create SerialMonitorResponse from dictionary."""
        return deserialize_dataclass(cls, data)
