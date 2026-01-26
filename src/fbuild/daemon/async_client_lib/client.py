"""
Async Client for fbuild daemon.

This module provides the main AsyncDaemonClient class for connecting to the daemon
using an asyncio-based protocol.
"""

from __future__ import annotations

import asyncio
import json
import logging
import time
import uuid
from typing import Any, Callable, Coroutine

from .connection import ConnectionManager
from .handlers import (
    FirmwareProtocolHandler,
    LockProtocolHandler,
    SerialProtocolHandler,
    SubscriptionProtocolHandler,
)
from .types import (
    ConnectionError,
    ConnectionState,
    DaemonClientError,
    MessageType,
    PendingRequest,
    TimeoutError,
)

# Default configuration
DEFAULT_HOST = "localhost"
DEFAULT_PORT = 9876  # Must match async_server.py DEFAULT_PORT
DEFAULT_REQUEST_TIMEOUT = 30.0
DEFAULT_HEARTBEAT_INTERVAL = 10.0


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
        ...     lock_acquired = await client.locks.acquire(
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
        reconnect_delay: float = 1.0,
        max_reconnect_delay: float = 60.0,
        reconnect_backoff_factor: float = 2.0,
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

        # Connection manager
        self._connection = ConnectionManager(
            client_id=self._client_id,
            auto_reconnect=auto_reconnect,
            reconnect_delay=reconnect_delay,
            max_reconnect_delay=max_reconnect_delay,
            reconnect_backoff_factor=reconnect_backoff_factor,
            logger=logging.getLogger(f"AsyncDaemonClient[{self._client_id[:8]}]"),
        )

        # Request tracking
        self._pending_requests: dict[str, PendingRequest] = {}
        self._request_id_counter = 0

        # Background tasks
        self._read_task: asyncio.Task[None] | None = None
        self._heartbeat_task: asyncio.Task[None] | None = None
        self._timeout_checker_task: asyncio.Task[None] | None = None

        # Event loop reference (for thread-safe operations)
        self._loop: asyncio.AbstractEventLoop | None = None

        # Logger
        self._logger = logging.getLogger(f"AsyncDaemonClient[{self._client_id[:8]}]")

        # Initialize protocol handlers
        self._subscription_handler = SubscriptionProtocolHandler(
            send_request=self._send_request,
            logger=self._logger,
        )
        self._lock_handler = LockProtocolHandler(
            send_request=self._send_request,
            logger=self._logger,
        )
        self._firmware_handler = FirmwareProtocolHandler(
            send_request=self._send_request,
            logger=self._logger,
        )
        self._serial_handler = SerialProtocolHandler(
            send_request=self._send_request,
            logger=self._logger,
        )

    @property
    def client_id(self) -> str:
        """Get the client ID."""
        return self._client_id

    @property
    def state(self) -> ConnectionState:
        """Get the current connection state."""
        return self._connection.state

    @property
    def is_connected(self) -> bool:
        """Check if client is connected."""
        return self._connection.is_connected

    @property
    def locks(self) -> LockProtocolHandler:
        """Get the lock protocol handler."""
        return self._lock_handler

    @property
    def firmware(self) -> FirmwareProtocolHandler:
        """Get the firmware protocol handler."""
        return self._firmware_handler

    @property
    def serial(self) -> SerialProtocolHandler:
        """Get the serial protocol handler."""
        return self._serial_handler

    @property
    def subscriptions(self) -> SubscriptionProtocolHandler:
        """Get the subscription protocol handler."""
        return self._subscription_handler

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
        if self._connection.is_connected:
            self._logger.warning("Already connected, disconnecting first")
            await self.disconnect()

        self._loop = asyncio.get_event_loop()

        await self._connection.connect(host, port, timeout)

        # Start background tasks
        self._read_task = asyncio.create_task(self._read_loop())
        self._heartbeat_task = asyncio.create_task(self._heartbeat_loop())
        self._timeout_checker_task = asyncio.create_task(self._timeout_checker_loop())

    async def disconnect(self, reason: str = "client requested") -> None:
        """Disconnect from the daemon server.

        Args:
            reason: Reason for disconnection (for logging)
        """
        if self._connection.state in (ConnectionState.DISCONNECTED, ConnectionState.CLOSED):
            return

        await self._connection.disconnect(reason)

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

        # Cancel pending requests
        for request_id, pending in list(self._pending_requests.items()):
            if not pending.future.done():
                pending.future.set_exception(ConnectionError("Disconnected"))
            del self._pending_requests[request_id]

        # Clear subscriptions
        self._subscription_handler.clear_subscriptions()

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
    # Convenience methods for locks (delegate to handler)
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
        """Acquire a configuration lock (convenience method).

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
        return await self._lock_handler.acquire(project_dir, environment, port, lock_type, timeout, description)

    async def release_lock(self, project_dir: str, environment: str, port: str) -> bool:
        """Release a configuration lock (convenience method).

        Args:
            project_dir: Absolute path to project directory
            environment: Build environment name
            port: Serial port for the configuration

        Returns:
            True if lock was released, False otherwise
        """
        return await self._lock_handler.release(project_dir, environment, port)

    async def get_lock_status(self, project_dir: str, environment: str, port: str) -> dict[str, Any]:
        """Get the status of a configuration lock (convenience method).

        Args:
            project_dir: Absolute path to project directory
            environment: Build environment name
            port: Serial port for the configuration

        Returns:
            Dictionary with lock status information
        """
        return await self._lock_handler.get_status(project_dir, environment, port)

    async def subscribe_lock_changes(
        self,
        callback: Callable[[dict[str, Any]], None] | Callable[[dict[str, Any]], Coroutine[Any, Any, None]],
        filter_key: str | None = None,
    ) -> str:
        """Subscribe to lock change events (convenience method).

        Args:
            callback: Function to call when lock changes occur
            filter_key: Optional key to filter events (e.g., specific port)

        Returns:
            Subscription ID for later unsubscription
        """
        return await self._subscription_handler.subscribe(
            MessageType.LOCK_SUBSCRIBE,
            "lock_change",
            callback,
            filter_key,
        )

    async def unsubscribe_lock_changes(self, subscription_id: str) -> bool:
        """Unsubscribe from lock change events (convenience method).

        Args:
            subscription_id: Subscription ID returned from subscribe_lock_changes

        Returns:
            True if unsubscribed successfully
        """
        return await self._subscription_handler.unsubscribe(MessageType.LOCK_UNSUBSCRIBE, subscription_id)

    # =========================================================================
    # Convenience methods for firmware (delegate to handler)
    # =========================================================================

    async def query_firmware(
        self,
        port: str,
        source_hash: str,
        build_flags_hash: str | None = None,
    ) -> dict[str, Any]:
        """Query if firmware is current on a device (convenience method).

        Args:
            port: Serial port of the device
            source_hash: Hash of the source files
            build_flags_hash: Hash of build flags (optional)

        Returns:
            Dictionary with firmware status
        """
        return await self._firmware_handler.query(port, source_hash, build_flags_hash)

    async def subscribe_firmware_changes(
        self,
        callback: Callable[[dict[str, Any]], None] | Callable[[dict[str, Any]], Coroutine[Any, Any, None]],
        port: str | None = None,
    ) -> str:
        """Subscribe to firmware change events (convenience method).

        Args:
            callback: Function to call when firmware changes
            port: Optional port to filter events

        Returns:
            Subscription ID for later unsubscription
        """
        return await self._subscription_handler.subscribe(
            MessageType.FIRMWARE_SUBSCRIBE,
            "firmware_change",
            callback,
            port,
        )

    async def unsubscribe_firmware_changes(self, subscription_id: str) -> bool:
        """Unsubscribe from firmware change events (convenience method).

        Args:
            subscription_id: Subscription ID from subscribe_firmware_changes

        Returns:
            True if unsubscribed successfully
        """
        return await self._subscription_handler.unsubscribe(MessageType.FIRMWARE_UNSUBSCRIBE, subscription_id)

    # =========================================================================
    # Convenience methods for serial (delegate to handler)
    # =========================================================================

    async def attach_serial(self, port: str, baud_rate: int = 115200, as_reader: bool = True) -> bool:
        """Attach to a serial session (convenience method).

        Args:
            port: Serial port to attach to
            baud_rate: Baud rate for the connection
            as_reader: Whether to attach as reader (True) or open port (False)

        Returns:
            True if attached successfully
        """
        return await self._serial_handler.attach(port, baud_rate, as_reader)

    async def detach_serial(self, port: str, close_port: bool = False) -> bool:
        """Detach from a serial session (convenience method).

        Args:
            port: Serial port to detach from
            close_port: Whether to close port if last reader

        Returns:
            True if detached successfully
        """
        return await self._serial_handler.detach(port, close_port)

    async def acquire_writer(self, port: str, timeout: float = 10.0) -> bool:
        """Acquire write access to a serial port (convenience method).

        Args:
            port: Serial port to acquire write access for
            timeout: Maximum time to wait for access

        Returns:
            True if write access acquired
        """
        return await self._serial_handler.acquire_writer(port, timeout)

    async def release_writer(self, port: str) -> bool:
        """Release write access to a serial port (convenience method).

        Args:
            port: Serial port to release write access for

        Returns:
            True if write access released
        """
        return await self._serial_handler.release_writer(port)

    async def write_serial(self, port: str, data: bytes, acquire_writer: bool = True) -> int:
        """Write data to a serial port (convenience method).

        Args:
            port: Serial port to write to
            data: Bytes to write
            acquire_writer: Whether to auto-acquire writer if not held

        Returns:
            Number of bytes written
        """
        return await self._serial_handler.write(port, data, acquire_writer)

    async def read_buffer(self, port: str, max_lines: int = 100) -> list[str]:
        """Read buffered serial output (convenience method).

        Args:
            port: Serial port to read from
            max_lines: Maximum number of lines to return

        Returns:
            List of output lines
        """
        return await self._serial_handler.read_buffer(port, max_lines)

    async def subscribe_serial_output(
        self,
        port: str,
        callback: Callable[[dict[str, Any]], None] | Callable[[dict[str, Any]], Coroutine[Any, Any, None]],
    ) -> str:
        """Subscribe to serial output events (convenience method).

        Args:
            port: Serial port to subscribe to
            callback: Function to call when serial output is received

        Returns:
            Subscription ID for later unsubscription
        """
        return await self._subscription_handler.subscribe(
            MessageType.SERIAL_SUBSCRIBE,
            "serial_output",
            callback,
            port,
        )

    async def unsubscribe_serial_output(self, subscription_id: str) -> bool:
        """Unsubscribe from serial output events (convenience method).

        Args:
            subscription_id: Subscription ID from subscribe_serial_output

        Returns:
            True if unsubscribed successfully
        """
        return await self._subscription_handler.unsubscribe(MessageType.SERIAL_UNSUBSCRIBE, subscription_id)

    # =========================================================================
    # Internal Methods
    # =========================================================================

    def _generate_request_id(self) -> str:
        """Generate a unique request ID."""
        self._request_id_counter += 1
        return f"{self._client_id[:8]}_{self._request_id_counter}_{int(time.time() * 1000)}"

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
            await self._connection.send_message(
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

    async def _read_loop(self) -> None:
        """Background task to read messages from daemon."""
        self._logger.debug("Read loop started")

        try:
            while not self._connection.shutdown_requested and self._connection.reader:
                try:
                    # Read line with timeout
                    line = await asyncio.wait_for(
                        self._connection.reader.readline(),
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
        if not self._connection.shutdown_requested and self._connection.auto_reconnect:
            self._logger.info("Connection lost, attempting reconnect")
            await self._connection.reconnect(self.connect)

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
            await self._subscription_handler.handle_broadcast(message)

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

    async def _heartbeat_loop(self) -> None:
        """Background task to send periodic heartbeats."""
        self._logger.debug("Heartbeat loop started")

        try:
            while not self._connection.shutdown_requested:
                await asyncio.sleep(self._heartbeat_interval)
                if self.is_connected:
                    await self._connection.send_heartbeat()

        except asyncio.CancelledError:
            self._logger.debug("Heartbeat loop cancelled")
            raise

    async def _timeout_checker_loop(self) -> None:
        """Background task to check for timed out requests."""
        self._logger.debug("Timeout checker loop started")

        try:
            while not self._connection.shutdown_requested:
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
