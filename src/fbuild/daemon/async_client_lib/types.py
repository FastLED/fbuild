"""
Shared types for async daemon client.

This module contains common types, enums, and dataclasses used across
the async client library.
"""

from __future__ import annotations

import asyncio
import time
from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Callable, Coroutine


class ConnectionState(Enum):
    """Connection state enumeration."""

    DISCONNECTED = "disconnected"
    CONNECTING = "connecting"
    CONNECTED = "connected"
    RECONNECTING = "reconnecting"
    CLOSED = "closed"


class MessageType(Enum):
    """Message types for client-daemon communication."""

    # Client connection management
    CLIENT_CONNECT = "client_connect"
    CLIENT_HEARTBEAT = "client_heartbeat"
    CLIENT_DISCONNECT = "client_disconnect"

    # Lock management
    LOCK_ACQUIRE = "lock_acquire"
    LOCK_RELEASE = "lock_release"
    LOCK_STATUS = "lock_status"
    LOCK_SUBSCRIBE = "lock_subscribe"
    LOCK_UNSUBSCRIBE = "lock_unsubscribe"

    # Firmware queries
    FIRMWARE_QUERY = "firmware_query"
    FIRMWARE_SUBSCRIBE = "firmware_subscribe"
    FIRMWARE_UNSUBSCRIBE = "firmware_unsubscribe"

    # Serial session management
    SERIAL_ATTACH = "serial_attach"
    SERIAL_DETACH = "serial_detach"
    SERIAL_ACQUIRE_WRITER = "serial_acquire_writer"
    SERIAL_RELEASE_WRITER = "serial_release_writer"
    SERIAL_WRITE = "serial_write"
    SERIAL_READ_BUFFER = "serial_read_buffer"
    SERIAL_SUBSCRIBE = "serial_subscribe"
    SERIAL_UNSUBSCRIBE = "serial_unsubscribe"

    # Response and broadcast types
    RESPONSE = "response"
    BROADCAST = "broadcast"
    ERROR = "error"


@dataclass
class PendingRequest:
    """Tracks a pending request awaiting response.

    Attributes:
        request_id: Unique identifier for the request
        message_type: Type of the request
        future: Future to resolve when response arrives
        timeout: Request timeout in seconds
        created_at: Timestamp when request was created
    """

    request_id: str
    message_type: MessageType
    future: asyncio.Future[dict[str, Any]]
    timeout: float
    created_at: float = field(default_factory=time.time)

    def is_expired(self) -> bool:
        """Check if request has timed out."""
        return (time.time() - self.created_at) > self.timeout


@dataclass
class Subscription:
    """Tracks an active subscription.

    Attributes:
        subscription_id: Unique identifier for the subscription
        event_type: Type of events being subscribed to
        callback: Function to call when event is received
        filter_key: Optional key to filter events (e.g., port name)
    """

    subscription_id: str
    event_type: str
    callback: Callable[[dict[str, Any]], None] | Callable[[dict[str, Any]], Coroutine[Any, Any, None]]
    filter_key: str | None = None


# Exception classes
class DaemonClientError(Exception):
    """Base exception for daemon client errors."""

    pass


class ConnectionError(DaemonClientError):
    """Error connecting to daemon."""

    pass


class TimeoutError(DaemonClientError):
    """Request timeout error."""

    pass


class ProtocolError(DaemonClientError):
    """Protocol/message format error."""

    pass
