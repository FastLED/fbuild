"""
JSON-based wire protocol for async daemon communication.

This module implements a length-prefixed JSON message protocol for reliable
communication over TCP streams. It provides:

- Message framing with 4-byte big-endian length prefix
- Message type enumeration for all daemon operations
- Base message structure with common fields
- Encode/decode functions for serialization
- Protocol constants for buffer sizes and timeouts

Wire Format:
    [4 bytes: message length (big-endian uint32)][N bytes: JSON payload]

Example message structure:
    {
        "type": "lock_acquire",
        "timestamp": 1234567890.123,
        "request_id": "req_abc123",
        "payload": { ... }
    }
"""

import json
import struct
import time
import uuid
from dataclasses import dataclass, field
from enum import Enum
from typing import Any

from fbuild.daemon.messages import (
    BuildRequest,
    ClientConnectRequest,
    ClientDisconnectRequest,
    ClientHeartbeatRequest,
    ClientResponse,
    DaemonStatus,
    DeployRequest,
    FirmwareQueryRequest,
    FirmwareQueryResponse,
    FirmwareRecordRequest,
    InstallDependenciesRequest,
    LockAcquireRequest,
    LockReleaseRequest,
    LockResponse,
    LockStatusRequest,
    MonitorRequest,
    SerialAttachRequest,
    SerialBufferRequest,
    SerialDetachRequest,
    SerialSessionResponse,
    SerialWriteRequest,
)

# =============================================================================
# Protocol Constants
# =============================================================================

# Message framing
LENGTH_PREFIX_SIZE = 4  # 4 bytes for big-endian uint32 length prefix
LENGTH_PREFIX_FORMAT = ">I"  # struct format for big-endian unsigned int
MAX_MESSAGE_SIZE = 16 * 1024 * 1024  # 16 MB maximum message size

# Buffer sizes
DEFAULT_READ_BUFFER_SIZE = 65536  # 64 KB read buffer
DEFAULT_WRITE_BUFFER_SIZE = 65536  # 64 KB write buffer

# Timeouts (in seconds)
DEFAULT_CONNECT_TIMEOUT = 10.0  # Connection establishment timeout
DEFAULT_READ_TIMEOUT = 30.0  # Read operation timeout
DEFAULT_WRITE_TIMEOUT = 10.0  # Write operation timeout
DEFAULT_RESPONSE_TIMEOUT = 60.0  # Time to wait for response
HEARTBEAT_INTERVAL = 10.0  # Interval between heartbeat messages
HEARTBEAT_TIMEOUT = 30.0  # Time before considering connection dead

# Retry settings
MAX_RETRY_ATTEMPTS = 3
RETRY_BACKOFF_BASE = 0.5  # Base delay for exponential backoff


# =============================================================================
# Message Types
# =============================================================================


class MessageType(Enum):
    """Enumeration of all protocol message types.

    Message types are grouped by functionality:
    - Connection management: connect, disconnect, heartbeat
    - Build operations: build, deploy, monitor, install_deps
    - Lock management: lock_acquire, lock_release, lock_status
    - Firmware ledger: firmware_query, firmware_record
    - Serial sessions: serial_attach, serial_detach, serial_write, serial_buffer
    - Status and responses: status, response, error
    """

    # Connection management
    CONNECT = "connect"
    DISCONNECT = "disconnect"
    HEARTBEAT = "heartbeat"

    # Build operations
    BUILD = "build"
    DEPLOY = "deploy"
    MONITOR = "monitor"
    INSTALL_DEPS = "install_deps"

    # Lock management
    LOCK_ACQUIRE = "lock_acquire"
    LOCK_RELEASE = "lock_release"
    LOCK_STATUS = "lock_status"

    # Firmware ledger
    FIRMWARE_QUERY = "firmware_query"
    FIRMWARE_RECORD = "firmware_record"

    # Serial sessions
    SERIAL_ATTACH = "serial_attach"
    SERIAL_DETACH = "serial_detach"
    SERIAL_WRITE = "serial_write"
    SERIAL_BUFFER = "serial_buffer"

    # Status and responses
    STATUS = "status"
    RESPONSE = "response"
    ERROR = "error"

    # Acknowledgments
    ACK = "ack"
    NACK = "nack"

    @classmethod
    def from_string(cls, value: str) -> "MessageType":
        """Convert string to MessageType.

        Args:
            value: String representation of message type.

        Returns:
            Corresponding MessageType enum value.

        Raises:
            ValueError: If value does not match any known message type.
        """
        try:
            return cls(value)
        except ValueError as e:
            raise ValueError(f"Unknown message type: {value}") from e


# =============================================================================
# Base Message Class
# =============================================================================


@dataclass
class ProtocolMessage:
    """Base protocol message with common fields.

    All messages in the protocol share these common fields:
    - type: The message type from MessageType enum
    - timestamp: Unix timestamp when message was created
    - request_id: Unique identifier for request/response correlation
    - payload: Message-specific data

    Attributes:
        type: Message type identifier.
        timestamp: Unix timestamp of message creation.
        request_id: Unique ID for correlating requests and responses.
        payload: Message-specific payload data.
    """

    type: MessageType
    timestamp: float = field(default_factory=time.time)
    request_id: str = field(default_factory=lambda: f"req_{uuid.uuid4().hex[:12]}")
    payload: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        """Convert message to dictionary for JSON serialization.

        Returns:
            Dictionary representation of the message.
        """
        return {
            "type": self.type.value,
            "timestamp": self.timestamp,
            "request_id": self.request_id,
            "payload": self.payload,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "ProtocolMessage":
        """Create ProtocolMessage from dictionary.

        Args:
            data: Dictionary containing message data.

        Returns:
            ProtocolMessage instance.

        Raises:
            KeyError: If required fields are missing.
            ValueError: If message type is invalid.
        """
        return cls(
            type=MessageType.from_string(data["type"]),
            timestamp=data.get("timestamp", time.time()),
            request_id=data.get("request_id", f"req_{uuid.uuid4().hex[:12]}"),
            payload=data.get("payload", {}),
        )


# =============================================================================
# Response Message
# =============================================================================


@dataclass
class ResponseMessage:
    """Response message for request/response correlation.

    Attributes:
        type: Always MessageType.RESPONSE.
        timestamp: Unix timestamp of response creation.
        request_id: ID of the request this responds to.
        success: Whether the operation succeeded.
        message: Human-readable status message.
        payload: Response-specific data.
        error_code: Optional error code if success is False.
    """

    request_id: str
    success: bool
    message: str = ""
    timestamp: float = field(default_factory=time.time)
    payload: dict[str, Any] = field(default_factory=dict)
    error_code: str | None = None

    def to_dict(self) -> dict[str, Any]:
        """Convert response to dictionary for JSON serialization.

        Returns:
            Dictionary representation of the response.
        """
        result = {
            "type": MessageType.RESPONSE.value,
            "timestamp": self.timestamp,
            "request_id": self.request_id,
            "success": self.success,
            "message": self.message,
            "payload": self.payload,
        }
        if self.error_code is not None:
            result["error_code"] = self.error_code
        return result

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "ResponseMessage":
        """Create ResponseMessage from dictionary.

        Args:
            data: Dictionary containing response data.

        Returns:
            ResponseMessage instance.
        """
        return cls(
            request_id=data["request_id"],
            success=data["success"],
            message=data.get("message", ""),
            timestamp=data.get("timestamp", time.time()),
            payload=data.get("payload", {}),
            error_code=data.get("error_code"),
        )


# =============================================================================
# Error Message
# =============================================================================


@dataclass
class ErrorMessage:
    """Error message for protocol-level errors.

    Attributes:
        request_id: ID of the request that caused the error (if known).
        error_code: Machine-readable error code.
        error_message: Human-readable error description.
        timestamp: Unix timestamp of error creation.
        details: Additional error details.
    """

    error_code: str
    error_message: str
    request_id: str | None = None
    timestamp: float = field(default_factory=time.time)
    details: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        """Convert error to dictionary for JSON serialization.

        Returns:
            Dictionary representation of the error.
        """
        result = {
            "type": MessageType.ERROR.value,
            "timestamp": self.timestamp,
            "error_code": self.error_code,
            "error_message": self.error_message,
            "details": self.details,
        }
        if self.request_id is not None:
            result["request_id"] = self.request_id
        return result

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "ErrorMessage":
        """Create ErrorMessage from dictionary.

        Args:
            data: Dictionary containing error data.

        Returns:
            ErrorMessage instance.
        """
        return cls(
            error_code=data["error_code"],
            error_message=data["error_message"],
            request_id=data.get("request_id"),
            timestamp=data.get("timestamp", time.time()),
            details=data.get("details", {}),
        )


# =============================================================================
# Protocol Error Codes
# =============================================================================


class ProtocolErrorCode:
    """Standard protocol error codes."""

    # Framing errors
    INVALID_LENGTH = "INVALID_LENGTH"
    MESSAGE_TOO_LARGE = "MESSAGE_TOO_LARGE"
    INCOMPLETE_MESSAGE = "INCOMPLETE_MESSAGE"

    # Parsing errors
    INVALID_JSON = "INVALID_JSON"
    MISSING_FIELD = "MISSING_FIELD"
    INVALID_MESSAGE_TYPE = "INVALID_MESSAGE_TYPE"
    INVALID_PAYLOAD = "INVALID_PAYLOAD"

    # Connection errors
    CONNECTION_CLOSED = "CONNECTION_CLOSED"
    CONNECTION_TIMEOUT = "CONNECTION_TIMEOUT"
    CONNECTION_REFUSED = "CONNECTION_REFUSED"

    # Request errors
    UNKNOWN_REQUEST = "UNKNOWN_REQUEST"
    DUPLICATE_REQUEST = "DUPLICATE_REQUEST"
    REQUEST_TIMEOUT = "REQUEST_TIMEOUT"

    # Internal errors
    INTERNAL_ERROR = "INTERNAL_ERROR"
    NOT_IMPLEMENTED = "NOT_IMPLEMENTED"


# =============================================================================
# Protocol Exceptions
# =============================================================================


class ProtocolError(Exception):
    """Base exception for protocol errors.

    Attributes:
        error_code: Machine-readable error code.
        message: Human-readable error description.
        request_id: ID of the related request (if known).
    """

    def __init__(
        self,
        error_code: str,
        message: str,
        request_id: str | None = None,
    ) -> None:
        """Initialize ProtocolError.

        Args:
            error_code: Machine-readable error code.
            message: Human-readable error description.
            request_id: ID of the related request (if known).
        """
        super().__init__(message)
        self.error_code = error_code
        self.request_id = request_id

    def to_error_message(self) -> ErrorMessage:
        """Convert exception to ErrorMessage.

        Returns:
            ErrorMessage representing this exception.
        """
        return ErrorMessage(
            error_code=self.error_code,
            error_message=str(self),
            request_id=self.request_id,
        )


class FramingError(ProtocolError):
    """Error in message framing (length prefix issues)."""

    def __init__(self, message: str) -> None:
        """Initialize FramingError.

        Args:
            message: Human-readable error description.
        """
        super().__init__(ProtocolErrorCode.INVALID_LENGTH, message)


class ParseError(ProtocolError):
    """Error parsing message content."""

    def __init__(self, message: str, request_id: str | None = None) -> None:
        """Initialize ParseError.

        Args:
            message: Human-readable error description.
            request_id: ID of the related request (if known).
        """
        super().__init__(ProtocolErrorCode.INVALID_JSON, message, request_id)


class MessageTooLargeError(ProtocolError):
    """Message exceeds maximum allowed size."""

    def __init__(self, size: int, max_size: int = MAX_MESSAGE_SIZE) -> None:
        """Initialize MessageTooLargeError.

        Args:
            size: Actual message size in bytes.
            max_size: Maximum allowed size in bytes.
        """
        super().__init__(
            ProtocolErrorCode.MESSAGE_TOO_LARGE,
            f"Message size {size} exceeds maximum {max_size}",
        )
        self.size = size
        self.max_size = max_size


# =============================================================================
# Encode/Decode Functions
# =============================================================================


def encode_message(message: dict[str, Any]) -> bytes:
    """Encode a message dictionary to wire format.

    Wire format: [4 bytes length prefix][JSON payload]

    Args:
        message: Message dictionary to encode.

    Returns:
        Bytes containing length-prefixed JSON message.

    Raises:
        MessageTooLargeError: If encoded message exceeds MAX_MESSAGE_SIZE.
    """
    # Serialize to JSON
    json_bytes = json.dumps(message, separators=(",", ":")).encode("utf-8")

    # Check size limit
    if len(json_bytes) > MAX_MESSAGE_SIZE:
        raise MessageTooLargeError(len(json_bytes))

    # Create length prefix
    length_prefix = struct.pack(LENGTH_PREFIX_FORMAT, len(json_bytes))

    return length_prefix + json_bytes


def encode_protocol_message(msg: ProtocolMessage | ResponseMessage | ErrorMessage) -> bytes:
    """Encode a protocol message object to wire format.

    Args:
        msg: Protocol message to encode.

    Returns:
        Bytes containing length-prefixed JSON message.

    Raises:
        MessageTooLargeError: If encoded message exceeds MAX_MESSAGE_SIZE.
    """
    return encode_message(msg.to_dict())


def decode_length_prefix(data: bytes) -> int:
    """Decode the length prefix from message header.

    Args:
        data: Bytes containing the length prefix (must be LENGTH_PREFIX_SIZE bytes).

    Returns:
        Message length in bytes.

    Raises:
        FramingError: If data is too short or contains invalid length.
    """
    if len(data) < LENGTH_PREFIX_SIZE:
        raise FramingError(f"Incomplete length prefix: got {len(data)} bytes, need {LENGTH_PREFIX_SIZE}")

    length = struct.unpack(LENGTH_PREFIX_FORMAT, data[:LENGTH_PREFIX_SIZE])[0]

    if length > MAX_MESSAGE_SIZE:
        raise MessageTooLargeError(length)

    return length


def decode_message(data: bytes) -> dict[str, Any]:
    """Decode a JSON message from bytes (without length prefix).

    Args:
        data: JSON-encoded message bytes.

    Returns:
        Decoded message dictionary.

    Raises:
        ParseError: If JSON parsing fails.
    """
    try:
        return json.loads(data.decode("utf-8"))
    except json.JSONDecodeError as e:
        raise ParseError(f"Invalid JSON: {e}") from e
    except UnicodeDecodeError as e:
        raise ParseError(f"Invalid UTF-8 encoding: {e}") from e


def decode_framed_message(data: bytes) -> tuple[dict[str, Any], int]:
    """Decode a length-prefixed message from bytes.

    This function handles the complete wire format including length prefix.

    Args:
        data: Bytes containing length prefix and JSON payload.

    Returns:
        Tuple of (decoded message dict, total bytes consumed).

    Raises:
        FramingError: If data is too short for length prefix.
        MessageTooLargeError: If message exceeds size limit.
        ParseError: If JSON parsing fails.
    """
    if len(data) < LENGTH_PREFIX_SIZE:
        raise FramingError(f"Incomplete data: got {len(data)} bytes, need at least {LENGTH_PREFIX_SIZE}")

    # Decode length
    length = decode_length_prefix(data)

    # Check we have complete message
    total_size = LENGTH_PREFIX_SIZE + length
    if len(data) < total_size:
        raise FramingError(f"Incomplete message: got {len(data)} bytes, need {total_size}")

    # Decode JSON payload
    json_data = data[LENGTH_PREFIX_SIZE:total_size]
    message = decode_message(json_data)

    return message, total_size


# =============================================================================
# Message Factory Functions
# =============================================================================


def create_request_message(
    message_type: MessageType,
    payload: dict[str, Any],
    request_id: str | None = None,
) -> ProtocolMessage:
    """Create a new request message.

    Args:
        message_type: Type of the request.
        payload: Request-specific payload data.
        request_id: Optional request ID (generated if not provided).

    Returns:
        ProtocolMessage instance.
    """
    return ProtocolMessage(
        type=message_type,
        payload=payload,
        request_id=request_id or f"req_{uuid.uuid4().hex[:12]}",
    )


def create_response_message(
    request_id: str,
    success: bool,
    message: str = "",
    payload: dict[str, Any] | None = None,
    error_code: str | None = None,
) -> ResponseMessage:
    """Create a response message for a request.

    Args:
        request_id: ID of the request being responded to.
        success: Whether the operation succeeded.
        message: Human-readable status message.
        payload: Response-specific payload data.
        error_code: Error code if success is False.

    Returns:
        ResponseMessage instance.
    """
    return ResponseMessage(
        request_id=request_id,
        success=success,
        message=message,
        payload=payload or {},
        error_code=error_code,
    )


def create_error_message(
    error_code: str,
    error_message: str,
    request_id: str | None = None,
    details: dict[str, Any] | None = None,
) -> ErrorMessage:
    """Create an error message.

    Args:
        error_code: Machine-readable error code.
        error_message: Human-readable error description.
        request_id: ID of the related request (if known).
        details: Additional error details.

    Returns:
        ErrorMessage instance.
    """
    return ErrorMessage(
        error_code=error_code,
        error_message=error_message,
        request_id=request_id,
        details=details or {},
    )


# =============================================================================
# Message Type to Payload Mapping
# =============================================================================


# Maps message types to their corresponding request/response classes from messages.py
MESSAGE_TYPE_MAP: dict[MessageType, type] = {
    MessageType.CONNECT: ClientConnectRequest,
    MessageType.DISCONNECT: ClientDisconnectRequest,
    MessageType.HEARTBEAT: ClientHeartbeatRequest,
    MessageType.BUILD: BuildRequest,
    MessageType.DEPLOY: DeployRequest,
    MessageType.MONITOR: MonitorRequest,
    MessageType.INSTALL_DEPS: InstallDependenciesRequest,
    MessageType.LOCK_ACQUIRE: LockAcquireRequest,
    MessageType.LOCK_RELEASE: LockReleaseRequest,
    MessageType.LOCK_STATUS: LockStatusRequest,
    MessageType.FIRMWARE_QUERY: FirmwareQueryRequest,
    MessageType.FIRMWARE_RECORD: FirmwareRecordRequest,
    MessageType.SERIAL_ATTACH: SerialAttachRequest,
    MessageType.SERIAL_DETACH: SerialDetachRequest,
    MessageType.SERIAL_WRITE: SerialWriteRequest,
    MessageType.SERIAL_BUFFER: SerialBufferRequest,
}

RESPONSE_TYPE_MAP: dict[MessageType, type] = {
    MessageType.CONNECT: ClientResponse,
    MessageType.DISCONNECT: ClientResponse,
    MessageType.HEARTBEAT: ClientResponse,
    MessageType.STATUS: DaemonStatus,
    MessageType.LOCK_ACQUIRE: LockResponse,
    MessageType.LOCK_RELEASE: LockResponse,
    MessageType.LOCK_STATUS: LockResponse,
    MessageType.FIRMWARE_QUERY: FirmwareQueryResponse,
    MessageType.SERIAL_ATTACH: SerialSessionResponse,
    MessageType.SERIAL_DETACH: SerialSessionResponse,
    MessageType.SERIAL_WRITE: SerialSessionResponse,
    MessageType.SERIAL_BUFFER: SerialSessionResponse,
}


def parse_typed_payload(
    message_type: MessageType,
    payload: dict[str, Any],
) -> Any:
    """Parse a payload dictionary into a typed message object.

    Uses the MESSAGE_TYPE_MAP to find the appropriate class for the
    message type and creates an instance from the payload.

    Args:
        message_type: Type of the message.
        payload: Payload dictionary to parse.

    Returns:
        Typed message object (e.g., BuildRequest, LockAcquireRequest).

    Raises:
        ValueError: If message type has no registered payload class.
        KeyError: If required fields are missing from payload.
    """
    payload_class = MESSAGE_TYPE_MAP.get(message_type)
    if payload_class is None:
        raise ValueError(f"No payload class registered for message type: {message_type}")

    return payload_class.from_dict(payload)


def parse_typed_response(
    message_type: MessageType,
    payload: dict[str, Any],
) -> Any:
    """Parse a response payload into a typed response object.

    Uses the RESPONSE_TYPE_MAP to find the appropriate class for the
    response type and creates an instance from the payload.

    Args:
        message_type: Type of the original request.
        payload: Response payload dictionary to parse.

    Returns:
        Typed response object (e.g., LockResponse, DaemonStatus).

    Raises:
        ValueError: If message type has no registered response class.
        KeyError: If required fields are missing from payload.
    """
    response_class = RESPONSE_TYPE_MAP.get(message_type)
    if response_class is None:
        raise ValueError(f"No response class registered for message type: {message_type}")

    return response_class.from_dict(payload)


# =============================================================================
# Utility Functions
# =============================================================================


def generate_request_id() -> str:
    """Generate a unique request ID.

    Returns:
        Unique request ID string in format "req_<hex>".
    """
    return f"req_{uuid.uuid4().hex[:12]}"


def is_request_type(message_type: MessageType) -> bool:
    """Check if a message type is a request type.

    Args:
        message_type: Message type to check.

    Returns:
        True if the type represents a request, False otherwise.
    """
    return message_type in MESSAGE_TYPE_MAP


def is_response_type(message_type: MessageType) -> bool:
    """Check if a message type is a response type.

    Args:
        message_type: Message type to check.

    Returns:
        True if the type represents a response.
    """
    return message_type in (MessageType.RESPONSE, MessageType.ACK, MessageType.NACK)


def is_error_type(message_type: MessageType) -> bool:
    """Check if a message type is an error type.

    Args:
        message_type: Message type to check.

    Returns:
        True if the type represents an error.
    """
    return message_type == MessageType.ERROR


def get_message_type_from_dict(data: dict[str, Any]) -> MessageType:
    """Extract and parse message type from a message dictionary.

    Args:
        data: Message dictionary.

    Returns:
        Parsed MessageType enum value.

    Raises:
        KeyError: If 'type' field is missing.
        ValueError: If type value is invalid.
    """
    if "type" not in data:
        raise KeyError("Missing 'type' field in message")

    return MessageType.from_string(data["type"])
