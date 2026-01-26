"""
Async Daemon Server - Asyncio-based TCP server for fbuild daemon communication.

This module provides an asyncio-based server for handling client connections
to the fbuild daemon. It supports:

- TCP connections on localhost (configurable port, default 9876)
- Optional Unix socket support for better performance on Unix systems
- Client connection lifecycle management (connect, heartbeat, disconnect)
- Message routing to appropriate handlers
- Broadcast support for sending messages to all or specific clients
- Subscription system for events (locks, firmware, serial)

The server is designed to run alongside the existing file-based daemon loop,
sharing the DaemonContext for thread-safe access to daemon state.

Example:
    >>> import asyncio
    >>> from fbuild.daemon.daemon_context import create_daemon_context
    >>> from fbuild.daemon.async_server import AsyncDaemonServer
    >>>
    >>> # Create daemon context
    >>> context = create_daemon_context(...)
    >>>
    >>> # Create and start server
    >>> server = AsyncDaemonServer(context, port=9876)
    >>> asyncio.run(server.start())
"""

from __future__ import annotations

import asyncio
import json
import logging
import sys
import threading
import time
import uuid
from dataclasses import dataclass, field
from enum import Enum
from pathlib import Path
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from fbuild.daemon.async_client import ClientConnectionManager
    from fbuild.daemon.configuration_lock import ConfigurationLockManager
    from fbuild.daemon.daemon_context import DaemonContext
    from fbuild.daemon.device_manager import DeviceManager
    from fbuild.daemon.firmware_ledger import FirmwareLedger
    from fbuild.daemon.shared_serial import SharedSerialManager

# Default server configuration
DEFAULT_PORT = 9876
DEFAULT_HOST = "127.0.0.1"
# Heartbeat timeout: clients must send heartbeat every ~1s; if missed for 4s, disconnect
# Per TASK.md requirement: "If daemon misses heartbeats for ~3–4s, daemon closes the connection"
DEFAULT_HEARTBEAT_TIMEOUT = 4.0
DEFAULT_READ_BUFFER_SIZE = 65536
DEFAULT_WRITE_TIMEOUT = 10.0

# Message delimiter for framing
MESSAGE_DELIMITER = b"\n"


class SubscriptionType(Enum):
    """Types of events clients can subscribe to."""

    LOCKS = "locks"  # Lock state changes
    FIRMWARE = "firmware"  # Firmware deployment events
    SERIAL = "serial"  # Serial port events
    DEVICES = "devices"  # Device lease events
    STATUS = "status"  # Daemon status updates
    ALL = "all"  # All events


class MessageType(Enum):
    """Types of messages that can be sent/received."""

    # Client lifecycle
    CONNECT = "connect"
    HEARTBEAT = "heartbeat"
    DISCONNECT = "disconnect"

    # Lock operations
    LOCK_ACQUIRE = "lock_acquire"
    LOCK_RELEASE = "lock_release"
    LOCK_STATUS = "lock_status"

    # Firmware operations
    FIRMWARE_QUERY = "firmware_query"
    FIRMWARE_RECORD = "firmware_record"

    # Serial operations
    SERIAL_ATTACH = "serial_attach"
    SERIAL_DETACH = "serial_detach"
    SERIAL_WRITE = "serial_write"
    SERIAL_READ = "serial_read"

    # Device operations
    DEVICE_LIST = "device_list"
    DEVICE_LEASE = "device_lease"
    DEVICE_RELEASE = "device_release"
    DEVICE_PREEMPT = "device_preempt"
    DEVICE_STATUS = "device_status"

    # Subscription
    SUBSCRIBE = "subscribe"
    UNSUBSCRIBE = "unsubscribe"

    # Responses
    RESPONSE = "response"
    ERROR = "error"
    BROADCAST = "broadcast"


@dataclass
class ClientConnection:
    """Represents a connected client with its state.

    Attributes:
        client_id: Unique identifier for the client (UUID string)
        reader: Asyncio stream reader for receiving messages
        writer: Asyncio stream writer for sending messages
        address: Client address (host, port) tuple
        connected_at: Unix timestamp when client connected
        last_heartbeat: Unix timestamp of last heartbeat received
        subscriptions: Set of event types the client is subscribed to
        metadata: Additional client metadata (pid, hostname, version, etc.)
        is_connected: Whether the client is currently connected
        lock: Lock for thread-safe writer access
    """

    client_id: str
    reader: asyncio.StreamReader
    writer: asyncio.StreamWriter
    address: tuple[str, int]
    connected_at: float = field(default_factory=time.time)
    last_heartbeat: float = field(default_factory=time.time)
    subscriptions: set[SubscriptionType] = field(default_factory=set)
    metadata: dict[str, Any] = field(default_factory=dict)
    is_connected: bool = True
    lock: asyncio.Lock = field(default_factory=asyncio.Lock)

    def is_alive(self, timeout_seconds: float = DEFAULT_HEARTBEAT_TIMEOUT) -> bool:
        """Check if client is still alive based on heartbeat timeout.

        Args:
            timeout_seconds: Maximum time since last heartbeat before considered dead.

        Returns:
            True if client is alive (heartbeat within timeout), False otherwise.
        """
        return self.is_connected and (time.time() - self.last_heartbeat) <= timeout_seconds

    def update_heartbeat(self) -> None:
        """Update the last heartbeat timestamp to current time."""
        self.last_heartbeat = time.time()

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return {
            "client_id": self.client_id,
            "address": f"{self.address[0]}:{self.address[1]}",
            "connected_at": self.connected_at,
            "last_heartbeat": self.last_heartbeat,
            "subscriptions": [s.value for s in self.subscriptions],
            "metadata": self.metadata,
            "is_connected": self.is_connected,
            "is_alive": self.is_alive(),
            "connection_duration": time.time() - self.connected_at,
            "time_since_heartbeat": time.time() - self.last_heartbeat,
        }


class AsyncDaemonServer:
    """Asyncio-based TCP server for the fbuild daemon.

    This server handles client connections and routes messages to appropriate
    handlers. It integrates with the existing daemon through the DaemonContext,
    using threading locks for thread-safe access to shared state.

    The server supports:
    - TCP connections on localhost (configurable port)
    - Optional Unix socket support on Unix systems
    - Client lifecycle management (connect, heartbeat, disconnect)
    - Message routing to handlers for locks, firmware, and serial operations
    - Broadcast messaging to all or subscribed clients
    - Graceful shutdown handling

    Example:
        >>> server = AsyncDaemonServer(context, port=9876)
        >>> # Start in background thread
        >>> server.start_in_background()
        >>> # ... daemon main loop runs ...
        >>> # Stop server on shutdown
        >>> server.stop()
    """

    def __init__(
        self,
        host: str = DEFAULT_HOST,
        port: int = DEFAULT_PORT,
        unix_socket_path: Path | None = None,
        heartbeat_timeout: float = DEFAULT_HEARTBEAT_TIMEOUT,
        # Individual managers can be passed if context is not available
        configuration_lock_manager: "ConfigurationLockManager | None" = None,
        firmware_ledger: "FirmwareLedger | None" = None,
        shared_serial_manager: "SharedSerialManager | None" = None,
        client_manager: "ClientConnectionManager | None" = None,
        device_manager: "DeviceManager | None" = None,
        # Full context can also be passed (takes precedence)
        context: "DaemonContext | None" = None,
    ) -> None:
        """Initialize the AsyncDaemonServer.

        The server can be initialized either with individual managers or with a
        full DaemonContext. If a context is provided, the individual managers
        are extracted from it.

        Args:
            host: Host to bind to (default: 127.0.0.1)
            port: Port to bind to (default: 9876)
            unix_socket_path: Optional Unix socket path for Unix systems
            heartbeat_timeout: Timeout in seconds for client heartbeats
            configuration_lock_manager: ConfigurationLockManager for lock operations
            firmware_ledger: FirmwareLedger for firmware tracking
            shared_serial_manager: SharedSerialManager for serial port operations
            client_manager: ClientConnectionManager for client tracking
            device_manager: DeviceManager for device leasing
            context: Full DaemonContext (if provided, individual managers are extracted)
        """
        self._host = host
        self._port = port
        self._unix_socket_path = unix_socket_path
        self._heartbeat_timeout = heartbeat_timeout

        # Extract managers from context if provided, otherwise use individual managers
        if context is not None:
            self._configuration_lock_manager = context.configuration_lock_manager
            self._firmware_ledger = context.firmware_ledger
            self._shared_serial_manager = context.shared_serial_manager
            self._client_manager = context.client_manager
            self._device_manager = getattr(context, "device_manager", None)
            self._context = context  # Keep reference for legacy access
        else:
            self._configuration_lock_manager = configuration_lock_manager
            self._firmware_ledger = firmware_ledger
            self._shared_serial_manager = shared_serial_manager
            self._client_manager = client_manager
            self._device_manager = device_manager
            self._context = None  # No full context available

        # Client tracking
        self._clients: dict[str, ClientConnection] = {}
        self._clients_lock = asyncio.Lock()

        # Server state
        self._server: asyncio.Server | None = None
        self._unix_server: asyncio.Server | None = None
        self._is_running = False
        self._shutdown_event: asyncio.Event | None = None

        # Background tasks
        self._heartbeat_task: asyncio.Task[None] | None = None
        self._background_thread: threading.Thread | None = None
        self._loop: asyncio.AbstractEventLoop | None = None

        # Message handlers - initialize using handler registry
        from fbuild.daemon.handlers import HandlerContext, create_handler_registry

        handler_context = HandlerContext(
            configuration_lock_manager=self._configuration_lock_manager,
            firmware_ledger=self._firmware_ledger,
            shared_serial_manager=self._shared_serial_manager,
            client_manager=self._client_manager,
            device_manager=self._device_manager,
            broadcast=self.broadcast,
            disconnect_client=self._disconnect_client,
        )

        self._handlers = create_handler_registry(
            context=handler_context,
            clients=self._clients,
            get_client_async=self.get_client_async,
        )

        logging.info(f"AsyncDaemonServer initialized (host={host}, port={port})")

    @property
    def is_running(self) -> bool:
        """Check if the server is currently running."""
        return self._is_running

    @property
    def client_count(self) -> int:
        """Get the number of connected clients."""
        return len(self._clients)

    async def start(self) -> None:
        """Start the async server and begin accepting connections.

        This method runs the event loop and blocks until shutdown is requested.
        For non-blocking operation, use start_in_background().
        """
        if self._is_running:
            logging.warning("AsyncDaemonServer already running")
            return

        self._is_running = True
        self._shutdown_event = asyncio.Event()

        try:
            # Start TCP server
            self._server = await asyncio.start_server(
                self._handle_client_connection,
                self._host,
                self._port,
            )
            addr = self._server.sockets[0].getsockname() if self._server.sockets else (self._host, self._port)
            logging.info(f"AsyncDaemonServer listening on {addr[0]}:{addr[1]}")

            # Start Unix socket server if path provided and on Unix
            if self._unix_socket_path and sys.platform != "win32":  # pragma: no cover
                await self._start_unix_socket_server()

            # Start heartbeat monitoring task
            self._heartbeat_task = asyncio.create_task(self._heartbeat_monitor())

            # Wait for shutdown signal
            await self._shutdown_event.wait()

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"AsyncDaemonServer error: {e}", exc_info=True)
            raise
        finally:
            await self._cleanup()

    async def _start_unix_socket_server(self) -> None:  # pragma: no cover
        """Start a Unix socket server for local connections (Unix only)."""
        if self._unix_socket_path is None:
            return

        try:
            # Remove existing socket file if present
            if self._unix_socket_path.exists():
                self._unix_socket_path.unlink()

            # start_unix_server is only available on Unix platforms
            start_unix_server = getattr(asyncio, "start_unix_server", None)
            if start_unix_server is None:
                logging.warning("Unix socket server not available on this platform")
                return

            self._unix_server = await start_unix_server(
                self._handle_client_connection,
                path=str(self._unix_socket_path),
            )
            logging.info(f"AsyncDaemonServer Unix socket listening on {self._unix_socket_path}")

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Failed to start Unix socket server: {e}")

    def start_in_background(self) -> None:
        """Start the server in a background thread.

        This method returns immediately, running the server's event loop
        in a separate thread. Use stop() to shut down the server.
        """
        if self._is_running:
            logging.warning("AsyncDaemonServer already running")
            return

        def run_loop() -> None:
            self._loop = asyncio.new_event_loop()
            asyncio.set_event_loop(self._loop)
            try:
                self._loop.run_until_complete(self.start())
            except KeyboardInterrupt:  # noqa: KBI002
                raise
            except Exception as e:
                logging.error(f"Background server error: {e}", exc_info=True)
            finally:
                self._loop.close()

        self._background_thread = threading.Thread(
            target=run_loop,
            name="AsyncDaemonServer",
            daemon=True,
        )
        self._background_thread.start()
        logging.info("AsyncDaemonServer started in background thread")

    def stop(self) -> None:
        """Stop the server and close all client connections.

        This method signals the server to shut down and waits for cleanup
        to complete. Safe to call from any thread.
        """
        if not self._is_running:
            return

        logging.info("Stopping AsyncDaemonServer...")

        if self._loop and self._shutdown_event:
            # Signal shutdown from the event loop thread
            self._loop.call_soon_threadsafe(self._shutdown_event.set)

        if self._background_thread and self._background_thread.is_alive():
            self._background_thread.join(timeout=5.0)
            if self._background_thread.is_alive():
                logging.warning("Background thread did not stop cleanly")

        self._is_running = False
        logging.info("AsyncDaemonServer stopped")

    async def _cleanup(self) -> None:
        """Clean up server resources and close connections."""
        logging.info("Cleaning up AsyncDaemonServer...")

        # Cancel heartbeat task
        if self._heartbeat_task and not self._heartbeat_task.done():
            self._heartbeat_task.cancel()
            try:
                await self._heartbeat_task
            except asyncio.CancelledError:
                pass

        # Close all client connections
        async with self._clients_lock:
            for client in list(self._clients.values()):
                await self._close_client(client, "Server shutting down")
            self._clients.clear()

        # Close TCP server
        if self._server:
            self._server.close()
            await self._server.wait_closed()
            self._server = None

        # Close Unix socket server
        if self._unix_server:
            self._unix_server.close()
            await self._unix_server.wait_closed()
            self._unix_server = None
            if self._unix_socket_path and self._unix_socket_path.exists():
                try:
                    self._unix_socket_path.unlink()
                except OSError:
                    pass

        self._is_running = False
        logging.info("AsyncDaemonServer cleanup complete")

    async def _handle_client_connection(
        self,
        reader: asyncio.StreamReader,
        writer: asyncio.StreamWriter,
    ) -> None:
        """Handle a new client connection.

        This coroutine is called for each new client connection. It manages
        the client lifecycle: registration, message processing, and cleanup.

        Args:
            reader: Asyncio stream reader for receiving messages
            writer: Asyncio stream writer for sending messages
        """
        addr = writer.get_extra_info("peername")
        client_id = str(uuid.uuid4())

        logging.info(f"New connection from {addr}, assigned client_id: {client_id}")

        # Create client connection object
        client = ClientConnection(
            client_id=client_id,
            reader=reader,
            writer=writer,
            address=addr if addr else ("unknown", 0),
        )

        # Register client
        async with self._clients_lock:
            self._clients[client_id] = client

        try:
            # Process messages until disconnection
            await self._process_client_messages(client)

        except asyncio.CancelledError:
            logging.debug(f"Client {client_id} connection cancelled")

        except KeyboardInterrupt:  # noqa: KBI002
            raise

        except Exception as e:
            logging.error(f"Error handling client {client_id}: {e}", exc_info=True)

        finally:
            # Clean up client
            await self._disconnect_client(client_id, "Connection closed")

    async def _process_client_messages(self, client: ClientConnection) -> None:
        """Process messages from a client until disconnection.

        Args:
            client: The client connection to process messages for
        """
        buffer = b""

        while client.is_connected:
            try:
                # Read data with timeout
                data = await asyncio.wait_for(
                    client.reader.read(DEFAULT_READ_BUFFER_SIZE),
                    timeout=self._heartbeat_timeout * 2,
                )

                if not data:
                    # Connection closed by client
                    logging.debug(f"Client {client.client_id} closed connection")
                    break

                buffer += data

                # Process complete messages (delimited by newline)
                while MESSAGE_DELIMITER in buffer:
                    message_bytes, buffer = buffer.split(MESSAGE_DELIMITER, 1)

                    if message_bytes:
                        await self._process_message(client, message_bytes)

            except asyncio.TimeoutError:
                # Check if client is still alive
                if not client.is_alive(self._heartbeat_timeout):
                    logging.warning(f"Client {client.client_id} heartbeat timeout")
                    break

            except asyncio.CancelledError:
                raise

            except KeyboardInterrupt:  # noqa: KBI002
                raise

            except Exception as e:
                logging.error(f"Error reading from client {client.client_id}: {e}")
                break

    async def _process_message(
        self,
        client: ClientConnection,
        message_bytes: bytes,
    ) -> None:
        """Process a single message from a client.

        Args:
            client: The client that sent the message
            message_bytes: Raw message bytes (JSON-encoded)
        """
        try:
            # Parse JSON message
            message = json.loads(message_bytes.decode("utf-8"))

            # Extract message type
            msg_type_str = message.get("type")
            if not msg_type_str:
                await self._send_error(client, "Missing message type")
                return

            try:
                msg_type = MessageType(msg_type_str)
            except ValueError:
                await self._send_error(client, f"Unknown message type: {msg_type_str}")
                return

            # Get handler for message type
            handler = self._handlers.get(msg_type)
            if not handler:
                await self._send_error(client, f"No handler for message type: {msg_type_str}")
                return

            # Call handler and send response
            logging.debug(f"Processing {msg_type.value} from client {client.client_id}")
            response = await handler.handle(client, message.get("data", {}))
            await self._send_response(client, response)

        except json.JSONDecodeError as e:
            logging.error(f"Invalid JSON from client {client.client_id}: {e}")
            await self._send_error(client, f"Invalid JSON: {e}")

        except KeyboardInterrupt:  # noqa: KBI002
            raise

        except Exception as e:
            logging.error(f"Error processing message from {client.client_id}: {e}", exc_info=True)
            await self._send_error(client, f"Error processing message: {e}")

    async def _send_message(
        self,
        client: ClientConnection,
        msg_type: MessageType,
        data: dict[str, Any],
    ) -> bool:
        """Send a message to a client.

        Args:
            client: The client to send to
            msg_type: Type of message
            data: Message data

        Returns:
            True if message was sent successfully, False otherwise
        """
        if not client.is_connected:
            return False

        message = {
            "type": msg_type.value,
            "data": data,
            "timestamp": time.time(),
        }

        try:
            message_bytes = json.dumps(message).encode("utf-8") + MESSAGE_DELIMITER

            async with client.lock:
                client.writer.write(message_bytes)
                await asyncio.wait_for(
                    client.writer.drain(),
                    timeout=DEFAULT_WRITE_TIMEOUT,
                )

            return True

        except asyncio.TimeoutError:
            logging.warning(f"Timeout sending to client {client.client_id}")
            return False

        except KeyboardInterrupt:  # noqa: KBI002
            raise

        except Exception as e:
            logging.error(f"Error sending to client {client.client_id}: {e}")
            return False

    async def _send_response(
        self,
        client: ClientConnection,
        data: dict[str, Any],
    ) -> bool:
        """Send a response message to a client.

        Args:
            client: The client to send to
            data: Response data

        Returns:
            True if response was sent successfully, False otherwise
        """
        return await self._send_message(client, MessageType.RESPONSE, data)

    async def _send_error(
        self,
        client: ClientConnection,
        error_message: str,
    ) -> bool:
        """Send an error message to a client.

        Args:
            client: The client to send to
            error_message: Error description

        Returns:
            True if error was sent successfully, False otherwise
        """
        return await self._send_message(
            client,
            MessageType.ERROR,
            {"success": False, "error": error_message, "timestamp": time.time()},
        )

    async def broadcast(
        self,
        event_type: SubscriptionType,
        data: dict[str, Any],
        exclude_client_id: str | None = None,
    ) -> int:
        """Broadcast a message to all subscribed clients.

        Args:
            event_type: Type of event being broadcast
            data: Event data
            exclude_client_id: Optional client ID to exclude from broadcast

        Returns:
            Number of clients the message was sent to
        """
        sent_count = 0
        broadcast_data = {
            "event_type": event_type.value,
            "data": data,
            "timestamp": time.time(),
        }

        async with self._clients_lock:
            for client in self._clients.values():
                if client.client_id == exclude_client_id:
                    continue

                # Check if client is subscribed to this event type
                if SubscriptionType.ALL in client.subscriptions or event_type in client.subscriptions:
                    if await self._send_message(client, MessageType.BROADCAST, broadcast_data):
                        sent_count += 1

        logging.debug(f"Broadcast {event_type.value} to {sent_count} clients")
        return sent_count

    async def send_to_client(
        self,
        client_id: str,
        data: dict[str, Any],
    ) -> bool:
        """Send a message to a specific client.

        Args:
            client_id: Target client ID
            data: Message data

        Returns:
            True if message was sent, False if client not found or send failed
        """
        async with self._clients_lock:
            client = self._clients.get(client_id)

        if not client:
            logging.warning(f"Client {client_id} not found for direct message")
            return False

        return await self._send_message(client, MessageType.RESPONSE, data)

    async def _close_client(
        self,
        client: ClientConnection,
        reason: str,
    ) -> None:
        """Close a client connection.

        Args:
            client: The client to close
            reason: Reason for closing the connection
        """
        if not client.is_connected:
            return

        client.is_connected = False
        logging.info(f"Closing client {client.client_id}: {reason}")

        try:
            client.writer.close()
            await asyncio.wait_for(client.writer.wait_closed(), timeout=2.0)
        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except (asyncio.TimeoutError, Exception) as e:
            logging.debug(f"Error closing client {client.client_id}: {e}")

    async def _disconnect_client(
        self,
        client_id: str,
        reason: str,
    ) -> None:
        """Disconnect a client and clean up resources.

        This method removes the client from tracking, closes the connection,
        and triggers cleanup callbacks in the DaemonContext.

        Args:
            client_id: Client ID to disconnect
            reason: Reason for disconnection
        """
        async with self._clients_lock:
            client = self._clients.pop(client_id, None)

        if not client:
            return

        # Close the connection
        await self._close_client(client, reason)

        # Trigger cleanup (thread-safe - individual managers handle their own locking)
        try:
            # Release configuration locks held by this client
            if self._configuration_lock_manager is not None:
                released = self._configuration_lock_manager.release_all_client_locks(client_id)
                if released > 0:
                    logging.info(f"Released {released} configuration locks for client {client_id}")

            # Release device leases held by this client
            if self._device_manager is not None:
                released = self._device_manager.release_all_client_leases(client_id)
                if released > 0:
                    logging.info(f"Released {released} device leases for client {client_id}")

            # Disconnect from shared serial sessions
            if self._shared_serial_manager is not None:
                self._shared_serial_manager.disconnect_client(client_id)

            # Unregister from client manager
            if self._client_manager is not None:
                self._client_manager.unregister_client(client_id)

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error during client cleanup for {client_id}: {e}")

        # Broadcast disconnection event
        await self.broadcast(
            SubscriptionType.STATUS,
            {
                "event": "client_disconnected",
                "client_id": client_id,
                "reason": reason,
            },
            exclude_client_id=client_id,
        )

    async def _heartbeat_monitor(self) -> None:
        """Background task to monitor client heartbeats and clean up dead clients."""
        logging.debug("Heartbeat monitor started")

        while self._is_running:
            try:
                await asyncio.sleep(self._heartbeat_timeout / 2)

                dead_clients: list[str] = []

                async with self._clients_lock:
                    for client_id, client in self._clients.items():
                        if not client.is_alive(self._heartbeat_timeout):
                            dead_clients.append(client_id)

                for client_id in dead_clients:
                    logging.warning(f"Client {client_id} heartbeat timeout, disconnecting")
                    await self._disconnect_client(client_id, "Heartbeat timeout")

            except asyncio.CancelledError:
                break

            except KeyboardInterrupt:  # noqa: KBI002
                raise

            except Exception as e:
                logging.error(f"Error in heartbeat monitor: {e}")

        logging.debug("Heartbeat monitor stopped")

    # =========================================================================
    # Status and Introspection
    # =========================================================================

    async def get_status(self) -> dict[str, Any]:
        """Get server status information.

        Returns:
            Dictionary with server status
        """
        async with self._clients_lock:
            clients_info = {client_id: client.to_dict() for client_id, client in self._clients.items()}

        return {
            "is_running": self._is_running,
            "host": self._host,
            "port": self._port,
            "client_count": len(clients_info),
            "clients": clients_info,
            "heartbeat_timeout": self._heartbeat_timeout,
        }

    def get_client(self, client_id: str) -> ClientConnection | None:
        """Get a client connection by ID.

        Note: This is not async-safe. Use with caution in async contexts.

        Args:
            client_id: The client ID to look up

        Returns:
            ClientConnection if found, None otherwise
        """
        return self._clients.get(client_id)

    async def get_client_async(self, client_id: str) -> ClientConnection | None:
        """Get a client connection by ID (async-safe).

        Args:
            client_id: The client ID to look up

        Returns:
            ClientConnection if found, None otherwise
        """
        async with self._clients_lock:
            return self._clients.get(client_id)
