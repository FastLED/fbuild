"""
Async Client Library for fbuild daemon.

This module provides asynchronous client classes for connecting to the fbuild daemon's
async server. It supports:

- Asyncio-based connection management
- Automatic reconnection with exponential backoff
- Request/response correlation with timeouts
- Event callback system for broadcasts
- Subscription management for events
- Both async and sync usage patterns

Example usage (async):
    >>> async def main():
    ...     client = AsyncDaemonClient()
    ...     await client.connect("localhost", 8765)
    ...     result = await client.acquire_lock("/project", "esp32", "/dev/ttyUSB0")
    ...     print(f"Lock acquired: {result}")
    ...     await client.disconnect()

Example usage (sync):
    >>> client = SyncDaemonClient()
    >>> client.connect("localhost", 8765)
    >>> result = client.acquire_lock("/project", "esp32", "/dev/ttyUSB0")
    >>> print(f"Lock acquired: {result}")
    >>> client.disconnect()
"""

from __future__ import annotations

import asyncio
import base64
import json
import logging
import os
import socket
import time
import uuid
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Callable, Coroutine

# Default configuration
DEFAULT_HOST = "localhost"
DEFAULT_PORT = 9876  # Must match async_server.py DEFAULT_PORT
DEFAULT_REQUEST_TIMEOUT = 30.0
DEFAULT_HEARTBEAT_INTERVAL = 10.0
DEFAULT_RECONNECT_DELAY = 1.0
DEFAULT_MAX_RECONNECT_DELAY = 60.0
DEFAULT_RECONNECT_BACKOFF_FACTOR = 2.0


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


class AsyncDaemonClient:
    """Asynchronous client for connecting to the fbuild daemon.

    This class provides a high-level async API for interacting with the daemon,
    including connection management, request/response handling, and event subscriptions.

    Features:
    - Uses asyncio streams (asyncio.open_connection)
    - Automatic reconnection with exponential backoff
    - Heartbeat sending (configurable interval, default 10 seconds)
    - Pending request tracking with timeouts
    - Event callback system for broadcasts
    - Thread-safe for use from sync code

    Example:
        >>> async with AsyncDaemonClient() as client:
        ...     await client.connect("localhost", 8765)
        ...     lock_acquired = await client.acquire_lock(
        ...         project_dir="/path/to/project",
        ...         environment="esp32",
        ...         port="/dev/ttyUSB0"
        ...     )
    """

    def __init__(
        self,
        client_id: str | None = None,
        heartbeat_interval: float = DEFAULT_HEARTBEAT_INTERVAL,
        request_timeout: float = DEFAULT_REQUEST_TIMEOUT,
        auto_reconnect: bool = True,
        reconnect_delay: float = DEFAULT_RECONNECT_DELAY,
        max_reconnect_delay: float = DEFAULT_MAX_RECONNECT_DELAY,
        reconnect_backoff_factor: float = DEFAULT_RECONNECT_BACKOFF_FACTOR,
    ) -> None:
        """Initialize the async daemon client.

        Args:
            client_id: Unique client identifier (auto-generated if None)
            heartbeat_interval: Interval between heartbeats in seconds
            request_timeout: Default timeout for requests in seconds
            auto_reconnect: Whether to automatically reconnect on disconnect
            reconnect_delay: Initial delay before reconnecting in seconds
            max_reconnect_delay: Maximum delay between reconnect attempts
            reconnect_backoff_factor: Factor to multiply delay on each retry
        """
        self._client_id = client_id or str(uuid.uuid4())
        self._heartbeat_interval = heartbeat_interval
        self._request_timeout = request_timeout
        self._auto_reconnect = auto_reconnect
        self._reconnect_delay = reconnect_delay
        self._max_reconnect_delay = max_reconnect_delay
        self._reconnect_backoff_factor = reconnect_backoff_factor

        # Connection state
        self._state = ConnectionState.DISCONNECTED
        self._host: str | None = None
        self._port: int | None = None
        self._reader: asyncio.StreamReader | None = None
        self._writer: asyncio.StreamWriter | None = None

        # Request tracking
        self._pending_requests: dict[str, PendingRequest] = {}
        self._request_id_counter = 0

        # Subscriptions
        self._subscriptions: dict[str, Subscription] = {}

        # Tasks
        self._read_task: asyncio.Task[None] | None = None
        self._heartbeat_task: asyncio.Task[None] | None = None
        self._timeout_checker_task: asyncio.Task[None] | None = None

        # Event loop reference (for thread-safe operations)
        self._loop: asyncio.AbstractEventLoop | None = None

        # Shutdown flag
        self._shutdown_requested = False

        # Logger
        self._logger = logging.getLogger(f"AsyncDaemonClient[{self._client_id[:8]}]")

    @property
    def client_id(self) -> str:
        """Get the client ID."""
        return self._client_id

    @property
    def state(self) -> ConnectionState:
        """Get the current connection state."""
        return self._state

    @property
    def is_connected(self) -> bool:
        """Check if client is connected."""
        return self._state == ConnectionState.CONNECTED

    async def __aenter__(self) -> "AsyncDaemonClient":
        """Async context manager entry."""
        return self

    async def __aexit__(
        self,
        exc_type: type | None,  # noqa: ARG002
        exc_val: Exception | None,  # noqa: ARG002
        exc_tb: Any,  # noqa: ARG002
    ) -> None:
        """Async context manager exit."""
        await self.disconnect()

    async def connect(
        self,
        host: str = DEFAULT_HOST,
        port: int = DEFAULT_PORT,
        timeout: float = 10.0,
    ) -> None:
        """Connect to the daemon server.

        Args:
            host: Daemon host address
            port: Daemon port number
            timeout: Connection timeout in seconds

        Raises:
            ConnectionError: If connection fails
        """
        if self._state == ConnectionState.CONNECTED:
            self._logger.warning("Already connected, disconnecting first")
            await self.disconnect()

        self._host = host
        self._port = port
        self._state = ConnectionState.CONNECTING
        self._shutdown_requested = False
        self._loop = asyncio.get_event_loop()

        try:
            self._logger.info(f"Connecting to daemon at {host}:{port}")

            # Open connection with timeout
            self._reader, self._writer = await asyncio.wait_for(
                asyncio.open_connection(host, port),
                timeout=timeout,
            )

            # Send client connect message
            await self._send_client_connect()

            # Start background tasks
            self._read_task = asyncio.create_task(self._read_loop())
            self._heartbeat_task = asyncio.create_task(self._heartbeat_loop())
            self._timeout_checker_task = asyncio.create_task(self._timeout_checker_loop())

            self._state = ConnectionState.CONNECTED
            self._logger.info(f"Connected to daemon at {host}:{port}")

        except asyncio.TimeoutError:
            self._state = ConnectionState.DISCONNECTED
            raise ConnectionError(f"Connection timeout connecting to {host}:{port}")
        except OSError as e:
            self._state = ConnectionState.DISCONNECTED
            raise ConnectionError(f"Failed to connect to {host}:{port}: {e}")
        except KeyboardInterrupt:  # noqa: KBI002
            self._state = ConnectionState.DISCONNECTED
            raise
        except Exception as e:
            self._state = ConnectionState.DISCONNECTED
            raise ConnectionError(f"Unexpected error connecting to {host}:{port}: {e}")

    async def disconnect(self, reason: str = "client requested") -> None:
        """Disconnect from the daemon server.

        Args:
            reason: Reason for disconnection (for logging)
        """
        if self._state in (ConnectionState.DISCONNECTED, ConnectionState.CLOSED):
            return

        self._logger.info(f"Disconnecting: {reason}")
        self._shutdown_requested = True
        self._state = ConnectionState.CLOSED

        # Send disconnect message (best effort)
        try:
            if self._writer and not self._writer.is_closing():
                await self._send_client_disconnect(reason)
        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            self._logger.debug(f"Error sending disconnect message: {e}")

        # Cancel background tasks
        if self._read_task and not self._read_task.done():
            self._read_task.cancel()
            try:
                await self._read_task
            except asyncio.CancelledError:
                pass

        if self._heartbeat_task and not self._heartbeat_task.done():
            self._heartbeat_task.cancel()
            try:
                await self._heartbeat_task
            except asyncio.CancelledError:
                pass

        if self._timeout_checker_task and not self._timeout_checker_task.done():
            self._timeout_checker_task.cancel()
            try:
                await self._timeout_checker_task
            except asyncio.CancelledError:
                pass

        # Close connection
        if self._writer and not self._writer.is_closing():
            self._writer.close()
            try:
                await self._writer.wait_closed()
            except KeyboardInterrupt:  # noqa: KBI002
                raise
            except Exception:
                pass

        self._reader = None
        self._writer = None
        self._state = ConnectionState.DISCONNECTED

        # Cancel pending requests
        for request_id, pending in list(self._pending_requests.items()):
            if not pending.future.done():
                pending.future.set_exception(ConnectionError("Disconnected"))
            del self._pending_requests[request_id]

        self._logger.info("Disconnected from daemon")

    async def wait_for_connection(self, timeout: float = 30.0) -> None:
        """Wait for the client to be connected.

        Args:
            timeout: Maximum time to wait in seconds

        Raises:
            TimeoutError: If connection not established within timeout
        """
        start_time = time.time()
        while not self.is_connected:
            if time.time() - start_time > timeout:
                raise TimeoutError(f"Connection not established within {timeout}s")
            await asyncio.sleep(0.1)

    # =========================================================================
    # Lock Management
    # =========================================================================

    async def acquire_lock(
        self,
        project_dir: str,
        environment: str,
        port: str,
        lock_type: str = "exclusive",
        timeout: float = 300.0,
        description: str = "",
    ) -> bool:
        """Acquire a configuration lock.

        Args:
            project_dir: Absolute path to project directory
            environment: Build environment name
            port: Serial port for the configuration
            lock_type: Type of lock ("exclusive" or "shared_read")
            timeout: Maximum time to wait for the lock in seconds
            description: Human-readable description of the operation

        Returns:
            True if lock was acquired, False otherwise
        """
        response = await self._send_request(
            MessageType.LOCK_ACQUIRE,
            {
                "project_dir": project_dir,
                "environment": environment,
                "port": port,
                "lock_type": lock_type,
                "timeout": timeout,
                "description": description,
            },
            timeout=timeout + 10.0,  # Add buffer for response
        )
        return response.get("success", False)

    async def release_lock(
        self,
        project_dir: str,
        environment: str,
        port: str,
    ) -> bool:
        """Release a configuration lock.

        Args:
            project_dir: Absolute path to project directory
            environment: Build environment name
            port: Serial port for the configuration

        Returns:
            True if lock was released, False otherwise
        """
        response = await self._send_request(
            MessageType.LOCK_RELEASE,
            {
                "project_dir": project_dir,
                "environment": environment,
                "port": port,
            },
        )
        return response.get("success", False)

    async def get_lock_status(
        self,
        project_dir: str,
        environment: str,
        port: str,
    ) -> dict[str, Any]:
        """Get the status of a configuration lock.

        Args:
            project_dir: Absolute path to project directory
            environment: Build environment name
            port: Serial port for the configuration

        Returns:
            Dictionary with lock status information
        """
        return await self._send_request(
            MessageType.LOCK_STATUS,
            {
                "project_dir": project_dir,
                "environment": environment,
                "port": port,
            },
        )

    async def subscribe_lock_changes(
        self,
        callback: Callable[[dict[str, Any]], None] | Callable[[dict[str, Any]], Coroutine[Any, Any, None]],
        filter_key: str | None = None,
    ) -> str:
        """Subscribe to lock change events.

        Args:
            callback: Function to call when lock changes occur
            filter_key: Optional key to filter events (e.g., specific port)

        Returns:
            Subscription ID for later unsubscription
        """
        subscription_id = str(uuid.uuid4())
        subscription = Subscription(
            subscription_id=subscription_id,
            event_type="lock_change",
            callback=callback,
            filter_key=filter_key,
        )
        self._subscriptions[subscription_id] = subscription

        await self._send_request(
            MessageType.LOCK_SUBSCRIBE,
            {
                "subscription_id": subscription_id,
                "filter_key": filter_key,
            },
        )

        return subscription_id

    async def unsubscribe_lock_changes(self, subscription_id: str) -> bool:
        """Unsubscribe from lock change events.

        Args:
            subscription_id: Subscription ID returned from subscribe_lock_changes

        Returns:
            True if unsubscribed successfully
        """
        if subscription_id not in self._subscriptions:
            return False

        await self._send_request(
            MessageType.LOCK_UNSUBSCRIBE,
            {"subscription_id": subscription_id},
        )

        del self._subscriptions[subscription_id]
        return True

    # =========================================================================
    # Firmware Queries
    # =========================================================================

    async def query_firmware(
        self,
        port: str,
        source_hash: str,
        build_flags_hash: str | None = None,
    ) -> dict[str, Any]:
        """Query if firmware is current on a device.

        Args:
            port: Serial port of the device
            source_hash: Hash of the source files
            build_flags_hash: Hash of build flags (optional)

        Returns:
            Dictionary with firmware status:
            - is_current: True if firmware matches
            - needs_redeploy: True if source changed
            - firmware_hash: Hash of deployed firmware
            - project_dir: Project directory of deployed firmware
            - environment: Environment of deployed firmware
            - upload_timestamp: When firmware was last uploaded
        """
        return await self._send_request(
            MessageType.FIRMWARE_QUERY,
            {
                "port": port,
                "source_hash": source_hash,
                "build_flags_hash": build_flags_hash,
            },
        )

    async def subscribe_firmware_changes(
        self,
        callback: Callable[[dict[str, Any]], None] | Callable[[dict[str, Any]], Coroutine[Any, Any, None]],
        port: str | None = None,
    ) -> str:
        """Subscribe to firmware change events.

        Args:
            callback: Function to call when firmware changes
            port: Optional port to filter events

        Returns:
            Subscription ID for later unsubscription
        """
        subscription_id = str(uuid.uuid4())
        subscription = Subscription(
            subscription_id=subscription_id,
            event_type="firmware_change",
            callback=callback,
            filter_key=port,
        )
        self._subscriptions[subscription_id] = subscription

        await self._send_request(
            MessageType.FIRMWARE_SUBSCRIBE,
            {
                "subscription_id": subscription_id,
                "port": port,
            },
        )

        return subscription_id

    async def unsubscribe_firmware_changes(self, subscription_id: str) -> bool:
        """Unsubscribe from firmware change events.

        Args:
            subscription_id: Subscription ID from subscribe_firmware_changes

        Returns:
            True if unsubscribed successfully
        """
        if subscription_id not in self._subscriptions:
            return False

        await self._send_request(
            MessageType.FIRMWARE_UNSUBSCRIBE,
            {"subscription_id": subscription_id},
        )

        del self._subscriptions[subscription_id]
        return True

    # =========================================================================
    # Serial Session Management
    # =========================================================================

    async def attach_serial(
        self,
        port: str,
        baud_rate: int = 115200,
        as_reader: bool = True,
    ) -> bool:
        """Attach to a serial session.

        Args:
            port: Serial port to attach to
            baud_rate: Baud rate for the connection
            as_reader: Whether to attach as reader (True) or open port (False)

        Returns:
            True if attached successfully
        """
        response = await self._send_request(
            MessageType.SERIAL_ATTACH,
            {
                "port": port,
                "baud_rate": baud_rate,
                "as_reader": as_reader,
            },
        )
        return response.get("success", False)

    async def detach_serial(
        self,
        port: str,
        close_port: bool = False,
    ) -> bool:
        """Detach from a serial session.

        Args:
            port: Serial port to detach from
            close_port: Whether to close port if last reader

        Returns:
            True if detached successfully
        """
        response = await self._send_request(
            MessageType.SERIAL_DETACH,
            {
                "port": port,
                "close_port": close_port,
            },
        )
        return response.get("success", False)

    async def acquire_writer(
        self,
        port: str,
        timeout: float = 10.0,
    ) -> bool:
        """Acquire write access to a serial port.

        Args:
            port: Serial port to acquire write access for
            timeout: Maximum time to wait for access

        Returns:
            True if write access acquired
        """
        response = await self._send_request(
            MessageType.SERIAL_ACQUIRE_WRITER,
            {
                "port": port,
                "timeout": timeout,
            },
            timeout=timeout + 5.0,
        )
        return response.get("success", False)

    async def release_writer(self, port: str) -> bool:
        """Release write access to a serial port.

        Args:
            port: Serial port to release write access for

        Returns:
            True if write access released
        """
        response = await self._send_request(
            MessageType.SERIAL_RELEASE_WRITER,
            {"port": port},
        )
        return response.get("success", False)

    async def write_serial(
        self,
        port: str,
        data: bytes,
        acquire_writer: bool = True,
    ) -> int:
        """Write data to a serial port.

        Args:
            port: Serial port to write to
            data: Bytes to write
            acquire_writer: Whether to auto-acquire writer if not held

        Returns:
            Number of bytes written
        """
        # Base64 encode the data for JSON transport
        encoded_data = base64.b64encode(data).decode("ascii")

        response = await self._send_request(
            MessageType.SERIAL_WRITE,
            {
                "port": port,
                "data": encoded_data,
                "acquire_writer": acquire_writer,
            },
        )

        if not response.get("success", False):
            return 0

        return response.get("bytes_written", 0)

    async def read_buffer(
        self,
        port: str,
        max_lines: int = 100,
    ) -> list[str]:
        """Read buffered serial output.

        Args:
            port: Serial port to read from
            max_lines: Maximum number of lines to return

        Returns:
            List of output lines
        """
        response = await self._send_request(
            MessageType.SERIAL_READ_BUFFER,
            {
                "port": port,
                "max_lines": max_lines,
            },
        )

        if not response.get("success", False):
            return []

        return response.get("lines", [])

    async def subscribe_serial_output(
        self,
        port: str,
        callback: Callable[[dict[str, Any]], None] | Callable[[dict[str, Any]], Coroutine[Any, Any, None]],
    ) -> str:
        """Subscribe to serial output events.

        Args:
            port: Serial port to subscribe to
            callback: Function to call when serial output is received

        Returns:
            Subscription ID for later unsubscription
        """
        subscription_id = str(uuid.uuid4())
        subscription = Subscription(
            subscription_id=subscription_id,
            event_type="serial_output",
            callback=callback,
            filter_key=port,
        )
        self._subscriptions[subscription_id] = subscription

        await self._send_request(
            MessageType.SERIAL_SUBSCRIBE,
            {
                "subscription_id": subscription_id,
                "port": port,
            },
        )

        return subscription_id

    async def unsubscribe_serial_output(self, subscription_id: str) -> bool:
        """Unsubscribe from serial output events.

        Args:
            subscription_id: Subscription ID from subscribe_serial_output

        Returns:
            True if unsubscribed successfully
        """
        if subscription_id not in self._subscriptions:
            return False

        await self._send_request(
            MessageType.SERIAL_UNSUBSCRIBE,
            {"subscription_id": subscription_id},
        )

        del self._subscriptions[subscription_id]
        return True

    # =========================================================================
    # Internal Methods
    # =========================================================================

    def _generate_request_id(self) -> str:
        """Generate a unique request ID."""
        self._request_id_counter += 1
        return f"{self._client_id[:8]}_{self._request_id_counter}_{int(time.time() * 1000)}"

    async def _send_message(self, message: dict[str, Any]) -> None:
        """Send a message to the daemon.

        Args:
            message: Message dictionary to send

        Raises:
            ConnectionError: If not connected or write fails
        """
        if not self._writer or self._writer.is_closing():
            raise ConnectionError("Not connected to daemon")

        try:
            # Add client_id and timestamp to all messages
            message["client_id"] = self._client_id
            message["timestamp"] = time.time()

            # Serialize and send with newline delimiter
            data = json.dumps(message) + "\n"
            self._writer.write(data.encode("utf-8"))
            await self._writer.drain()

            self._logger.debug(f"Sent message: {message.get('type', 'unknown')}")

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            self._logger.error(f"Error sending message: {e}")
            raise ConnectionError(f"Failed to send message: {e}")

    async def _send_request(
        self,
        message_type: MessageType,
        payload: dict[str, Any],
        timeout: float | None = None,
    ) -> dict[str, Any]:
        """Send a request and wait for response.

        Args:
            message_type: Type of request
            payload: Request payload
            timeout: Request timeout (uses default if None)

        Returns:
            Response dictionary

        Raises:
            TimeoutError: If request times out
            ConnectionError: If not connected
        """
        if not self.is_connected:
            raise ConnectionError("Not connected to daemon")

        timeout = timeout or self._request_timeout
        request_id = self._generate_request_id()

        # Create future for response
        future: asyncio.Future[dict[str, Any]] = asyncio.Future()

        # Track pending request
        pending = PendingRequest(
            request_id=request_id,
            message_type=message_type,
            future=future,
            timeout=timeout,
        )
        self._pending_requests[request_id] = pending

        try:
            # Send request
            await self._send_message(
                {
                    "type": message_type.value,
                    "request_id": request_id,
                    **payload,
                }
            )

            # Wait for response with timeout
            return await asyncio.wait_for(future, timeout=timeout)

        except asyncio.TimeoutError:
            self._logger.warning(f"Request {request_id} timed out after {timeout}s")
            raise TimeoutError(f"Request timed out after {timeout}s")

        finally:
            # Clean up pending request
            self._pending_requests.pop(request_id, None)

    async def _send_client_connect(self) -> None:
        """Send client connect message."""
        await self._send_message(
            {
                "type": MessageType.CLIENT_CONNECT.value,
                "pid": os.getpid(),
                "hostname": socket.gethostname(),
                "version": "1.0.0",  # TODO: Get from package version
            }
        )

    async def _send_client_disconnect(self, reason: str) -> None:
        """Send client disconnect message."""
        await self._send_message(
            {
                "type": MessageType.CLIENT_DISCONNECT.value,
                "reason": reason,
            }
        )

    async def _send_heartbeat(self) -> None:
        """Send heartbeat message."""
        try:
            await self._send_message({"type": MessageType.CLIENT_HEARTBEAT.value})
        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            self._logger.warning(f"Failed to send heartbeat: {e}")

    async def _read_loop(self) -> None:
        """Background task to read messages from daemon."""
        self._logger.debug("Read loop started")

        try:
            while not self._shutdown_requested and self._reader:
                try:
                    # Read line with timeout
                    line = await asyncio.wait_for(
                        self._reader.readline(),
                        timeout=self._heartbeat_interval * 3,
                    )

                    if not line:
                        # Connection closed
                        self._logger.warning("Connection closed by server")
                        break

                    # Parse message
                    try:
                        message = json.loads(line.decode("utf-8"))
                        await self._handle_message(message)
                    except json.JSONDecodeError as e:
                        self._logger.warning(f"Invalid JSON received: {e}")

                except asyncio.TimeoutError:
                    # No data received, check connection
                    self._logger.debug("Read timeout, connection may be idle")
                    continue

                except asyncio.CancelledError:
                    self._logger.debug("Read loop cancelled")
                    raise

                except KeyboardInterrupt:  # noqa: KBI002
                    raise

                except Exception as e:
                    self._logger.error(f"Read error: {e}")
                    break

        except asyncio.CancelledError:
            self._logger.debug("Read loop task cancelled")
            raise

        # Handle disconnection
        if not self._shutdown_requested and self._auto_reconnect:
            self._logger.info("Connection lost, attempting reconnect")
            await self._reconnect()

    async def _handle_message(self, message: dict[str, Any]) -> None:
        """Handle an incoming message.

        Args:
            message: Parsed message dictionary
        """
        msg_type = message.get("type", "")

        if msg_type == MessageType.RESPONSE.value:
            # Handle response to pending request
            request_id = message.get("request_id")
            if request_id and request_id in self._pending_requests:
                pending = self._pending_requests[request_id]
                if not pending.future.done():
                    pending.future.set_result(message)
            else:
                self._logger.warning(f"Received response for unknown request: {request_id}")

        elif msg_type == MessageType.BROADCAST.value:
            # Handle broadcast event
            await self._handle_broadcast(message)

        elif msg_type == MessageType.ERROR.value:
            # Handle error message
            error_msg = message.get("message", "Unknown error")
            request_id = message.get("request_id")
            if request_id and request_id in self._pending_requests:
                pending = self._pending_requests[request_id]
                if not pending.future.done():
                    pending.future.set_exception(DaemonClientError(error_msg))
            else:
                self._logger.error(f"Received error: {error_msg}")

        else:
            self._logger.debug(f"Received message of type: {msg_type}")

    async def _handle_broadcast(self, message: dict[str, Any]) -> None:
        """Handle a broadcast event.

        Args:
            message: Broadcast message
        """
        event_type = message.get("event_type", "")
        filter_key = message.get("filter_key")

        for subscription in self._subscriptions.values():
            if subscription.event_type == event_type:
                # Check filter
                if subscription.filter_key is not None and subscription.filter_key != filter_key:
                    continue

                # Call callback
                try:
                    result = subscription.callback(message)
                    if asyncio.iscoroutine(result):
                        await result
                except KeyboardInterrupt:  # noqa: KBI002
                    raise
                except Exception as e:
                    self._logger.error(f"Error in subscription callback: {e}")

    async def _heartbeat_loop(self) -> None:
        """Background task to send periodic heartbeats."""
        self._logger.debug("Heartbeat loop started")

        try:
            while not self._shutdown_requested:
                await asyncio.sleep(self._heartbeat_interval)
                if self.is_connected:
                    await self._send_heartbeat()

        except asyncio.CancelledError:
            self._logger.debug("Heartbeat loop cancelled")
            raise

    async def _timeout_checker_loop(self) -> None:
        """Background task to check for timed out requests."""
        self._logger.debug("Timeout checker loop started")

        try:
            while not self._shutdown_requested:
                await asyncio.sleep(1.0)

                # Check for expired requests
                expired = []
                for request_id, pending in list(self._pending_requests.items()):
                    if pending.is_expired() and not pending.future.done():
                        expired.append((request_id, pending))

                # Cancel expired requests
                for request_id, pending in expired:
                    self._logger.warning(f"Request {request_id} expired")
                    pending.future.set_exception(TimeoutError(f"Request timed out after {pending.timeout}s"))
                    self._pending_requests.pop(request_id, None)

        except asyncio.CancelledError:
            self._logger.debug("Timeout checker loop cancelled")
            raise

    async def _reconnect(self) -> None:
        """Attempt to reconnect to the daemon."""
        if not self._host or not self._port:
            self._logger.error("Cannot reconnect: no host/port configured")
            return

        self._state = ConnectionState.RECONNECTING
        delay = self._reconnect_delay

        while not self._shutdown_requested and self._auto_reconnect:
            self._logger.info(f"Attempting reconnect in {delay}s")
            await asyncio.sleep(delay)

            try:
                await self.connect(self._host, self._port)
                self._logger.info("Reconnection successful")
                return

            except ConnectionError as e:
                self._logger.warning(f"Reconnection failed: {e}")
                delay = min(delay * self._reconnect_backoff_factor, self._max_reconnect_delay)

        self._state = ConnectionState.DISCONNECTED


class SyncDaemonClient:
    """Synchronous wrapper around AsyncDaemonClient for use from sync code.

    This class provides a synchronous API that internally uses the async client
    by running operations in a dedicated event loop thread.

    Example:
        >>> client = SyncDaemonClient()
        >>> client.connect("localhost", 8765)
        >>> lock_acquired = client.acquire_lock("/project", "esp32", "/dev/ttyUSB0")
        >>> print(f"Lock acquired: {lock_acquired}")
        >>> client.disconnect()
    """

    def __init__(
        self,
        client_id: str | None = None,
        heartbeat_interval: float = DEFAULT_HEARTBEAT_INTERVAL,
        request_timeout: float = DEFAULT_REQUEST_TIMEOUT,
        auto_reconnect: bool = True,
    ) -> None:
        """Initialize the sync daemon client.

        Args:
            client_id: Unique client identifier (auto-generated if None)
            heartbeat_interval: Interval between heartbeats in seconds
            request_timeout: Default timeout for requests in seconds
            auto_reconnect: Whether to automatically reconnect on disconnect
        """
        self._async_client = AsyncDaemonClient(
            client_id=client_id,
            heartbeat_interval=heartbeat_interval,
            request_timeout=request_timeout,
            auto_reconnect=auto_reconnect,
        )
        self._loop: asyncio.AbstractEventLoop | None = None
        self._thread_executor = ThreadPoolExecutor(max_workers=1)
        self._loop_thread_started = False

    def __enter__(self) -> "SyncDaemonClient":
        """Context manager entry."""
        return self

    def __exit__(
        self,
        _exc_type: type | None,
        _exc_val: Exception | None,
        _exc_tb: Any,
    ) -> None:
        """Context manager exit."""
        self.disconnect()
        self.close()

    @property
    def client_id(self) -> str:
        """Get the client ID."""
        return self._async_client.client_id

    @property
    def is_connected(self) -> bool:
        """Check if client is connected."""
        return self._async_client.is_connected

    def _ensure_loop(self) -> asyncio.AbstractEventLoop:
        """Ensure event loop is running and return it."""
        if self._loop is None or not self._loop.is_running():
            self._loop = asyncio.new_event_loop()
            # Start loop in background thread
            import threading

            self._loop_thread = threading.Thread(
                target=self._run_event_loop,
                daemon=True,
            )
            self._loop_thread.start()
            self._loop_thread_started = True

        return self._loop

    def _run_event_loop(self) -> None:
        """Run the event loop in a background thread."""
        if self._loop:
            asyncio.set_event_loop(self._loop)
            self._loop.run_forever()

    def _run_async(self, coro: Coroutine[Any, Any, Any]) -> Any:
        """Run an async coroutine from sync code.

        Args:
            coro: Coroutine to run

        Returns:
            Result of the coroutine
        """
        loop = self._ensure_loop()
        future = asyncio.run_coroutine_threadsafe(coro, loop)
        return future.result()

    def connect(
        self,
        host: str = DEFAULT_HOST,
        port: int = DEFAULT_PORT,
        timeout: float = 10.0,
    ) -> None:
        """Connect to the daemon server.

        Args:
            host: Daemon host address
            port: Daemon port number
            timeout: Connection timeout in seconds
        """
        self._run_async(self._async_client.connect(host, port, timeout))

    def disconnect(self, reason: str = "client requested") -> None:
        """Disconnect from the daemon server.

        Args:
            reason: Reason for disconnection
        """
        try:
            self._run_async(self._async_client.disconnect(reason))
        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception:
            pass

    def close(self) -> None:
        """Close the client and cleanup resources."""
        if self._loop and self._loop.is_running():
            self._loop.call_soon_threadsafe(self._loop.stop)

        self._thread_executor.shutdown(wait=False)

    def wait_for_connection(self, timeout: float = 30.0) -> None:
        """Wait for connection to be established.

        Args:
            timeout: Maximum time to wait
        """
        self._run_async(self._async_client.wait_for_connection(timeout))

    # =========================================================================
    # Lock Management
    # =========================================================================

    def acquire_lock(
        self,
        project_dir: str,
        environment: str,
        port: str,
        lock_type: str = "exclusive",
        timeout: float = 300.0,
        description: str = "",
    ) -> bool:
        """Acquire a configuration lock.

        Args:
            project_dir: Absolute path to project directory
            environment: Build environment name
            port: Serial port for the configuration
            lock_type: Type of lock ("exclusive" or "shared_read")
            timeout: Maximum time to wait for the lock
            description: Human-readable description

        Returns:
            True if lock was acquired
        """
        return self._run_async(self._async_client.acquire_lock(project_dir, environment, port, lock_type, timeout, description))

    def release_lock(
        self,
        project_dir: str,
        environment: str,
        port: str,
    ) -> bool:
        """Release a configuration lock.

        Args:
            project_dir: Absolute path to project directory
            environment: Build environment name
            port: Serial port for the configuration

        Returns:
            True if lock was released
        """
        return self._run_async(self._async_client.release_lock(project_dir, environment, port))

    def get_lock_status(
        self,
        project_dir: str,
        environment: str,
        port: str,
    ) -> dict[str, Any]:
        """Get the status of a configuration lock.

        Args:
            project_dir: Absolute path to project directory
            environment: Build environment name
            port: Serial port for the configuration

        Returns:
            Dictionary with lock status information
        """
        return self._run_async(self._async_client.get_lock_status(project_dir, environment, port))

    def subscribe_lock_changes(
        self,
        callback: Callable[[dict[str, Any]], None],
        filter_key: str | None = None,
    ) -> str:
        """Subscribe to lock change events.

        Args:
            callback: Function to call when lock changes
            filter_key: Optional key to filter events

        Returns:
            Subscription ID
        """
        return self._run_async(self._async_client.subscribe_lock_changes(callback, filter_key))

    # =========================================================================
    # Firmware Queries
    # =========================================================================

    def query_firmware(
        self,
        port: str,
        source_hash: str,
        build_flags_hash: str | None = None,
    ) -> dict[str, Any]:
        """Query if firmware is current on a device.

        Args:
            port: Serial port of the device
            source_hash: Hash of the source files
            build_flags_hash: Hash of build flags

        Returns:
            Dictionary with firmware status
        """
        return self._run_async(self._async_client.query_firmware(port, source_hash, build_flags_hash))

    def subscribe_firmware_changes(
        self,
        callback: Callable[[dict[str, Any]], None],
        port: str | None = None,
    ) -> str:
        """Subscribe to firmware change events.

        Args:
            callback: Function to call when firmware changes
            port: Optional port to filter events

        Returns:
            Subscription ID
        """
        return self._run_async(self._async_client.subscribe_firmware_changes(callback, port))

    # =========================================================================
    # Serial Session Management
    # =========================================================================

    def attach_serial(
        self,
        port: str,
        baud_rate: int = 115200,
        as_reader: bool = True,
    ) -> bool:
        """Attach to a serial session.

        Args:
            port: Serial port to attach to
            baud_rate: Baud rate for the connection
            as_reader: Whether to attach as reader

        Returns:
            True if attached successfully
        """
        return self._run_async(self._async_client.attach_serial(port, baud_rate, as_reader))

    def detach_serial(
        self,
        port: str,
        close_port: bool = False,
    ) -> bool:
        """Detach from a serial session.

        Args:
            port: Serial port to detach from
            close_port: Whether to close port if last reader

        Returns:
            True if detached successfully
        """
        return self._run_async(self._async_client.detach_serial(port, close_port))

    def acquire_writer(
        self,
        port: str,
        timeout: float = 10.0,
    ) -> bool:
        """Acquire write access to a serial port.

        Args:
            port: Serial port
            timeout: Maximum time to wait

        Returns:
            True if write access acquired
        """
        return self._run_async(self._async_client.acquire_writer(port, timeout))

    def release_writer(self, port: str) -> bool:
        """Release write access to a serial port.

        Args:
            port: Serial port

        Returns:
            True if released
        """
        return self._run_async(self._async_client.release_writer(port))

    def write_serial(
        self,
        port: str,
        data: bytes,
        acquire_writer: bool = True,
    ) -> int:
        """Write data to a serial port.

        Args:
            port: Serial port
            data: Bytes to write
            acquire_writer: Whether to auto-acquire writer

        Returns:
            Number of bytes written
        """
        return self._run_async(self._async_client.write_serial(port, data, acquire_writer))

    def read_buffer(
        self,
        port: str,
        max_lines: int = 100,
    ) -> list[str]:
        """Read buffered serial output.

        Args:
            port: Serial port
            max_lines: Maximum lines to return

        Returns:
            List of output lines
        """
        return self._run_async(self._async_client.read_buffer(port, max_lines))

    def subscribe_serial_output(
        self,
        port: str,
        callback: Callable[[dict[str, Any]], None],
    ) -> str:
        """Subscribe to serial output events.

        Args:
            port: Serial port
            callback: Function to call on output

        Returns:
            Subscription ID
        """
        return self._run_async(self._async_client.subscribe_serial_output(port, callback))


# Convenience function to create a client
def create_client(
    sync: bool = False,
    **kwargs: Any,
) -> AsyncDaemonClient | SyncDaemonClient:
    """Create a daemon client.

    Args:
        sync: If True, create a SyncDaemonClient, otherwise AsyncDaemonClient
        **kwargs: Arguments to pass to client constructor

    Returns:
        Client instance
    """
    if sync:
        return SyncDaemonClient(**kwargs)
    return AsyncDaemonClient(**kwargs)
