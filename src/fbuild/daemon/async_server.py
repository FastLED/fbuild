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
import base64
import json
import logging
import sys
import threading
import time
import uuid
from dataclasses import dataclass, field
from enum import Enum
from pathlib import Path
from typing import TYPE_CHECKING, Any, Callable, Coroutine

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

        # Message handlers
        self._handlers: dict[MessageType, Callable[[ClientConnection, dict[str, Any]], Coroutine[Any, Any, dict[str, Any]]]] = {
            MessageType.CONNECT: self._handle_connect,
            MessageType.HEARTBEAT: self._handle_heartbeat,
            MessageType.DISCONNECT: self._handle_disconnect,
            MessageType.LOCK_ACQUIRE: self._handle_lock_acquire,
            MessageType.LOCK_RELEASE: self._handle_lock_release,
            MessageType.LOCK_STATUS: self._handle_lock_status,
            MessageType.FIRMWARE_QUERY: self._handle_firmware_query,
            MessageType.FIRMWARE_RECORD: self._handle_firmware_record,
            MessageType.SERIAL_ATTACH: self._handle_serial_attach,
            MessageType.SERIAL_DETACH: self._handle_serial_detach,
            MessageType.SERIAL_WRITE: self._handle_serial_write,
            MessageType.SERIAL_READ: self._handle_serial_read,
            MessageType.DEVICE_LIST: self._handle_device_list,
            MessageType.DEVICE_LEASE: self._handle_device_lease,
            MessageType.DEVICE_RELEASE: self._handle_device_release,
            MessageType.DEVICE_PREEMPT: self._handle_device_preempt,
            MessageType.DEVICE_STATUS: self._handle_device_status,
            MessageType.SUBSCRIBE: self._handle_subscribe,
            MessageType.UNSUBSCRIBE: self._handle_unsubscribe,
        }

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
            response = await handler(client, message.get("data", {}))
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
    # Message Handlers - Client Lifecycle
    # =========================================================================

    async def _handle_connect(
        self,
        client: ClientConnection,
        data: dict[str, Any],
    ) -> dict[str, Any]:
        """Handle client connect message.

        Args:
            client: The client connection
            data: Connect request data (pid, hostname, version, etc.)

        Returns:
            Response data with connection confirmation
        """
        # Update client metadata
        client.metadata = {
            "pid": data.get("pid"),
            "hostname": data.get("hostname", ""),
            "version": data.get("version", ""),
        }
        client.update_heartbeat()

        # Register with DaemonContext client manager
        if self._client_manager is not None:
            try:
                self._client_manager.register_client(
                    client_id=client.client_id,
                    pid=data.get("pid", 0),
                    metadata=client.metadata,
                )
            except KeyboardInterrupt:  # noqa: KBI002
                raise
            except Exception as e:
                logging.error(f"Error registering client {client.client_id}: {e}")

        logging.info(f"Client {client.client_id} connected (pid={data.get('pid')})")

        # Broadcast connection event
        await self.broadcast(
            SubscriptionType.STATUS,
            {
                "event": "client_connected",
                "client_id": client.client_id,
                "metadata": client.metadata,
            },
            exclude_client_id=client.client_id,
        )

        return {
            "success": True,
            "client_id": client.client_id,
            "message": "Connected successfully",
            "total_clients": len(self._clients),
        }

    async def _handle_heartbeat(
        self,
        client: ClientConnection,
        data: dict[str, Any],  # noqa: ARG002
    ) -> dict[str, Any]:
        """Handle client heartbeat message.

        Args:
            client: The client connection
            data: Heartbeat data (unused but required for handler signature)

        Returns:
            Response acknowledging the heartbeat
        """
        client.update_heartbeat()

        # Update in DaemonContext client manager
        if self._client_manager is not None:
            self._client_manager.heartbeat(client.client_id)

        logging.debug(f"Heartbeat from client {client.client_id}")

        return {
            "success": True,
            "message": "Heartbeat acknowledged",
            "timestamp": time.time(),
        }

    async def _handle_disconnect(
        self,
        client: ClientConnection,
        data: dict[str, Any],
    ) -> dict[str, Any]:
        """Handle graceful client disconnect message.

        Args:
            client: The client connection
            data: Disconnect data (optional reason)

        Returns:
            Response confirming disconnection
        """
        reason = data.get("reason", "Client requested disconnect")
        logging.info(f"Client {client.client_id} disconnecting: {reason}")

        # Schedule disconnection after response is sent
        asyncio.create_task(self._disconnect_client(client.client_id, reason))

        return {
            "success": True,
            "message": "Disconnect acknowledged",
        }

    # =========================================================================
    # Message Handlers - Lock Operations
    # =========================================================================

    async def _handle_lock_acquire(
        self,
        client: ClientConnection,
        data: dict[str, Any],
    ) -> dict[str, Any]:
        """Handle lock acquire request.

        Args:
            client: The client connection
            data: Lock request data (project_dir, environment, port, lock_type, etc.)

        Returns:
            Response with lock acquisition result
        """
        from fbuild.daemon.messages import LockType

        project_dir = data.get("project_dir", "")
        environment = data.get("environment", "")
        port = data.get("port", "")
        lock_type_str = data.get("lock_type", "exclusive")
        description = data.get("description", "")
        timeout = data.get("timeout", 300.0)

        config_key = (project_dir, environment, port)

        try:
            lock_type = LockType(lock_type_str)
        except ValueError:
            return {
                "success": False,
                "message": f"Invalid lock type: {lock_type_str}",
            }

        # Check that configuration lock manager is available
        if self._configuration_lock_manager is None:
            return {
                "success": False,
                "message": "Lock manager not available",
            }

        # Acquire lock (thread-safe through ConfigurationLockManager)
        try:
            if lock_type == LockType.EXCLUSIVE:
                acquired = self._configuration_lock_manager.acquire_exclusive(
                    config_key,
                    client.client_id,
                    description,
                    timeout,
                )
            else:  # SHARED_READ
                acquired = self._configuration_lock_manager.acquire_shared_read(
                    config_key,
                    client.client_id,
                    description,
                )

            if acquired:
                logging.info(f"Client {client.client_id} acquired {lock_type.value} lock for {config_key}")

                # Broadcast lock change
                await self.broadcast(
                    SubscriptionType.LOCKS,
                    {
                        "event": "lock_acquired",
                        "client_id": client.client_id,
                        "config_key": {"project_dir": project_dir, "environment": environment, "port": port},
                        "lock_type": lock_type.value,
                    },
                )

                return {
                    "success": True,
                    "message": f"{lock_type.value} lock acquired",
                    "lock_state": f"locked_{lock_type.value}",
                }
            else:
                lock_status = self._configuration_lock_manager.get_lock_status(config_key)
                return {
                    "success": False,
                    "message": "Lock not available",
                    "lock_state": lock_status.get("state", "unknown"),
                    "holder_count": lock_status.get("holder_count", 0),
                    "waiting_count": lock_status.get("waiting_count", 0),
                }

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error acquiring lock for {client.client_id}: {e}")
            return {
                "success": False,
                "message": f"Lock acquisition error: {e}",
            }

    async def _handle_lock_release(
        self,
        client: ClientConnection,
        data: dict[str, Any],
    ) -> dict[str, Any]:
        """Handle lock release request.

        Args:
            client: The client connection
            data: Lock release data (project_dir, environment, port)

        Returns:
            Response with lock release result
        """
        project_dir = data.get("project_dir", "")
        environment = data.get("environment", "")
        port = data.get("port", "")

        config_key = (project_dir, environment, port)

        # Check that configuration lock manager is available
        if self._configuration_lock_manager is None:
            return {
                "success": False,
                "message": "Lock manager not available",
            }

        try:
            released = self._configuration_lock_manager.release(
                config_key,
                client.client_id,
            )

            if released:
                logging.info(f"Client {client.client_id} released lock for {config_key}")

                # Broadcast lock change
                await self.broadcast(
                    SubscriptionType.LOCKS,
                    {
                        "event": "lock_released",
                        "client_id": client.client_id,
                        "config_key": {"project_dir": project_dir, "environment": environment, "port": port},
                    },
                )

                return {
                    "success": True,
                    "message": "Lock released",
                    "lock_state": "unlocked",
                }
            else:
                return {
                    "success": False,
                    "message": "Client does not hold this lock",
                }

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error releasing lock for {client.client_id}: {e}")
            return {
                "success": False,
                "message": f"Lock release error: {e}",
            }

    async def _handle_lock_status(
        self,
        client: ClientConnection,  # noqa: ARG002
        data: dict[str, Any],
    ) -> dict[str, Any]:
        """Handle lock status query.

        Args:
            client: The client connection (unused but required for handler signature)
            data: Lock query data (project_dir, environment, port)

        Returns:
            Response with current lock status
        """
        project_dir = data.get("project_dir", "")
        environment = data.get("environment", "")
        port = data.get("port", "")

        config_key = (project_dir, environment, port)

        # Check that configuration lock manager is available
        if self._configuration_lock_manager is None:
            return {
                "success": False,
                "message": "Lock manager not available",
            }

        try:
            lock_status = self._configuration_lock_manager.get_lock_status(config_key)
            return {
                "success": True,
                **lock_status,
            }

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error getting lock status: {e}")
            return {
                "success": False,
                "message": f"Lock status error: {e}",
            }

    # =========================================================================
    # Message Handlers - Firmware Operations
    # =========================================================================

    async def _handle_firmware_query(
        self,
        client: ClientConnection,  # noqa: ARG002
        data: dict[str, Any],
    ) -> dict[str, Any]:
        """Handle firmware query request.

        Args:
            client: The client connection (unused but required for handler signature)
            data: Query data (port, source_hash, build_flags_hash)

        Returns:
            Response with firmware status
        """
        port = data.get("port", "")
        source_hash = data.get("source_hash", "")
        build_flags_hash = data.get("build_flags_hash")

        # Check that firmware ledger is available
        if self._firmware_ledger is None:
            return {
                "success": False,
                "is_current": False,
                "needs_redeploy": True,
                "message": "Firmware ledger not available",
            }

        try:
            entry = self._firmware_ledger.get_deployment(port)

            if entry is None:
                return {
                    "success": True,
                    "is_current": False,
                    "needs_redeploy": True,
                    "message": "No firmware deployment recorded for this port",
                }

            is_current = entry.source_hash == source_hash
            if build_flags_hash and entry.build_flags_hash != build_flags_hash:
                is_current = False

            return {
                "success": True,
                "is_current": is_current,
                "needs_redeploy": not is_current,
                "firmware_hash": entry.firmware_hash,
                "project_dir": entry.project_dir,
                "environment": entry.environment,
                "upload_timestamp": entry.upload_timestamp,
                "message": "Firmware current" if is_current else "Firmware needs update",
            }

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error querying firmware: {e}")
            return {
                "success": False,
                "is_current": False,
                "needs_redeploy": True,
                "message": f"Firmware query error: {e}",
            }

    async def _handle_firmware_record(
        self,
        client: ClientConnection,
        data: dict[str, Any],
    ) -> dict[str, Any]:
        """Handle firmware record request.

        Args:
            client: The client connection
            data: Record data (port, firmware_hash, source_hash, project_dir, environment)

        Returns:
            Response confirming record creation
        """
        port = data.get("port", "")
        firmware_hash = data.get("firmware_hash", "")
        source_hash = data.get("source_hash", "")
        project_dir = data.get("project_dir", "")
        environment = data.get("environment", "")
        build_flags_hash = data.get("build_flags_hash")

        # Check that firmware ledger is available
        if self._firmware_ledger is None:
            return {
                "success": False,
                "message": "Firmware ledger not available",
            }

        try:
            self._firmware_ledger.record_deployment(
                port=port,
                firmware_hash=firmware_hash,
                source_hash=source_hash,
                project_dir=project_dir,
                environment=environment,
                build_flags_hash=build_flags_hash,
            )

            logging.info(f"Recorded firmware deployment to {port} by client {client.client_id}")

            # Broadcast firmware event
            await self.broadcast(
                SubscriptionType.FIRMWARE,
                {
                    "event": "firmware_deployed",
                    "port": port,
                    "project_dir": project_dir,
                    "environment": environment,
                    "client_id": client.client_id,
                },
            )

            return {
                "success": True,
                "message": "Firmware deployment recorded",
            }

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error recording firmware: {e}")
            return {
                "success": False,
                "message": f"Firmware record error: {e}",
            }

    # =========================================================================
    # Message Handlers - Serial Operations
    # =========================================================================

    async def _handle_serial_attach(
        self,
        client: ClientConnection,
        data: dict[str, Any],
    ) -> dict[str, Any]:
        """Handle serial attach request.

        Args:
            client: The client connection
            data: Attach data (port, baud_rate, as_reader)

        Returns:
            Response with attach result
        """
        port = data.get("port", "")
        baud_rate = data.get("baud_rate", 115200)
        as_reader = data.get("as_reader", True)

        # Check that shared serial manager is available
        if self._shared_serial_manager is None:
            return {
                "success": False,
                "message": "Serial manager not available",
            }

        try:
            # Open port if not already open
            opened = self._shared_serial_manager.open_port(
                port,
                baud_rate,
                client.client_id,
            )

            if as_reader:
                attached = self._shared_serial_manager.attach_reader(
                    port,
                    client.client_id,
                )
            else:
                attached = opened

            if attached:
                session_info = self._shared_serial_manager.get_session_info(port)

                # Broadcast serial event
                await self.broadcast(
                    SubscriptionType.SERIAL,
                    {
                        "event": "client_attached",
                        "port": port,
                        "client_id": client.client_id,
                        "as_reader": as_reader,
                    },
                )

                return {
                    "success": True,
                    "message": "Attached to serial port",
                    "is_open": True,
                    "reader_count": session_info.get("reader_count", 0) if session_info else 0,
                    "has_writer": session_info.get("writer_client_id") is not None if session_info else False,
                }
            else:
                return {
                    "success": False,
                    "message": "Failed to attach to serial port",
                }

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error attaching to serial: {e}")
            return {
                "success": False,
                "message": f"Serial attach error: {e}",
            }

    async def _handle_serial_detach(
        self,
        client: ClientConnection,
        data: dict[str, Any],
    ) -> dict[str, Any]:
        """Handle serial detach request.

        Args:
            client: The client connection
            data: Detach data (port, close_port)

        Returns:
            Response with detach result
        """
        port = data.get("port", "")
        close_port = data.get("close_port", False)

        # Check that shared serial manager is available
        if self._shared_serial_manager is None:
            return {
                "success": False,
                "message": "Serial manager not available",
            }

        try:
            detached = self._shared_serial_manager.detach_reader(
                port,
                client.client_id,
            )

            if close_port:
                self._shared_serial_manager.close_port(port, client.client_id)

            if detached:
                # Broadcast serial event
                await self.broadcast(
                    SubscriptionType.SERIAL,
                    {
                        "event": "client_detached",
                        "port": port,
                        "client_id": client.client_id,
                    },
                )

                return {
                    "success": True,
                    "message": "Detached from serial port",
                }
            else:
                return {
                    "success": False,
                    "message": "Client not attached to this port",
                }

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error detaching from serial: {e}")
            return {
                "success": False,
                "message": f"Serial detach error: {e}",
            }

    async def _handle_serial_write(
        self,
        client: ClientConnection,
        data: dict[str, Any],
    ) -> dict[str, Any]:
        """Handle serial write request.

        Args:
            client: The client connection
            data: Write data (port, data as base64, acquire_writer)

        Returns:
            Response with write result
        """
        port = data.get("port", "")
        data_b64 = data.get("data", "")
        acquire_writer = data.get("acquire_writer", True)

        # Check that shared serial manager is available
        if self._shared_serial_manager is None:
            return {
                "success": False,
                "message": "Serial manager not available",
            }

        try:
            # Decode base64 data
            write_data = base64.b64decode(data_b64)

            # Acquire writer if needed
            if acquire_writer:
                acquired = self._shared_serial_manager.acquire_writer(
                    port,
                    client.client_id,
                    timeout=5.0,
                )
                if not acquired:
                    return {
                        "success": False,
                        "message": "Could not acquire writer access",
                    }

            # Write data
            bytes_written = self._shared_serial_manager.write(
                port,
                client.client_id,
                write_data,
            )

            # Release writer if we acquired it
            if acquire_writer:
                self._shared_serial_manager.release_writer(port, client.client_id)

            if bytes_written >= 0:
                return {
                    "success": True,
                    "message": f"Wrote {bytes_written} bytes",
                    "bytes_written": bytes_written,
                }
            else:
                return {
                    "success": False,
                    "message": "Write failed",
                }

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error writing to serial: {e}")
            return {
                "success": False,
                "message": f"Serial write error: {e}",
            }

    async def _handle_serial_read(
        self,
        client: ClientConnection,
        data: dict[str, Any],
    ) -> dict[str, Any]:
        """Handle serial read (buffer) request.

        Args:
            client: The client connection
            data: Read data (port, max_lines)

        Returns:
            Response with buffered lines
        """
        port = data.get("port", "")
        max_lines = data.get("max_lines", 100)

        # Check that shared serial manager is available
        if self._shared_serial_manager is None:
            return {
                "success": False,
                "message": "Serial manager not available",
                "lines": [],
            }

        try:
            lines = self._shared_serial_manager.read_buffer(
                port,
                client.client_id,
                max_lines,
            )

            session_info = self._shared_serial_manager.get_session_info(port)

            return {
                "success": True,
                "message": f"Read {len(lines)} lines",
                "lines": lines,
                "buffer_size": session_info.get("buffer_size", 0) if session_info else 0,
            }

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error reading serial buffer: {e}")
            return {
                "success": False,
                "message": f"Serial read error: {e}",
                "lines": [],
            }

    # =========================================================================
    # Message Handlers - Subscription
    # =========================================================================

    async def _handle_subscribe(
        self,
        client: ClientConnection,
        data: dict[str, Any],
    ) -> dict[str, Any]:
        """Handle subscription request.

        Args:
            client: The client connection
            data: Subscribe data (event_types list)

        Returns:
            Response confirming subscription
        """
        event_types = data.get("event_types", [])

        for event_type_str in event_types:
            try:
                event_type = SubscriptionType(event_type_str)
                client.subscriptions.add(event_type)
            except ValueError:
                logging.warning(f"Unknown subscription type: {event_type_str}")

        logging.debug(f"Client {client.client_id} subscribed to {[s.value for s in client.subscriptions]}")

        return {
            "success": True,
            "message": "Subscribed",
            "subscriptions": [s.value for s in client.subscriptions],
        }

    async def _handle_unsubscribe(
        self,
        client: ClientConnection,
        data: dict[str, Any],
    ) -> dict[str, Any]:
        """Handle unsubscription request.

        Args:
            client: The client connection
            data: Unsubscribe data (event_types list)

        Returns:
            Response confirming unsubscription
        """
        event_types = data.get("event_types", [])

        for event_type_str in event_types:
            try:
                event_type = SubscriptionType(event_type_str)
                client.subscriptions.discard(event_type)
            except ValueError:
                pass

        logging.debug(f"Client {client.client_id} now subscribed to {[s.value for s in client.subscriptions]}")

        return {
            "success": True,
            "message": "Unsubscribed",
            "subscriptions": [s.value for s in client.subscriptions],
        }

    # =========================================================================
    # Message Handlers - Device Operations
    # =========================================================================

    async def _handle_device_list(
        self,
        client: ClientConnection,  # noqa: ARG002
        data: dict[str, Any],
    ) -> dict[str, Any]:
        """Handle device list request.

        Args:
            client: The client connection (unused but required for handler signature)
            data: List request data (include_disconnected, refresh)

        Returns:
            Response with device list
        """
        include_disconnected = data.get("include_disconnected", False)
        refresh = data.get("refresh", False)

        # Check that device manager is available
        if self._device_manager is None:
            return {
                "success": False,
                "message": "Device manager not available",
                "devices": [],
                "total_devices": 0,
                "connected_devices": 0,
                "total_leases": 0,
            }

        try:
            # Refresh device inventory if requested
            if refresh:
                self._device_manager.refresh_devices()

            # Get device status
            all_status = self._device_manager.get_all_leases()

            # Filter devices based on include_disconnected
            devices = []
            for _device_id, device_state in all_status.get("devices", {}).items():
                if include_disconnected or device_state.get("is_connected", False):
                    devices.append(device_state)

            return {
                "success": True,
                "message": f"Found {len(devices)} device(s)",
                "devices": devices,
                "total_devices": all_status.get("total_devices", 0),
                "connected_devices": all_status.get("connected_devices", 0),
                "total_leases": all_status.get("total_leases", 0),
            }

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error listing devices: {e}")
            return {
                "success": False,
                "message": f"Device list error: {e}",
                "devices": [],
                "total_devices": 0,
                "connected_devices": 0,
                "total_leases": 0,
            }

    async def _handle_device_lease(
        self,
        client: ClientConnection,
        data: dict[str, Any],
    ) -> dict[str, Any]:
        """Handle device lease request.

        Args:
            client: The client connection
            data: Lease request data (device_id, lease_type, description, allows_monitors, timeout)

        Returns:
            Response with lease result
        """
        from fbuild.daemon.device_manager import LeaseType

        device_id = data.get("device_id", "")
        lease_type_str = data.get("lease_type", "exclusive")
        description = data.get("description", "")
        allows_monitors = data.get("allows_monitors", True)
        timeout = data.get("timeout", 300.0)

        if not device_id:
            return {
                "success": False,
                "message": "device_id is required",
            }

        # Check that device manager is available
        if self._device_manager is None:
            return {
                "success": False,
                "message": "Device manager not available",
            }

        try:
            lease_type = LeaseType(lease_type_str)
        except ValueError:
            return {
                "success": False,
                "message": f"Invalid lease type: {lease_type_str}. Must be 'exclusive' or 'monitor'",
            }

        try:
            if lease_type == LeaseType.EXCLUSIVE:
                lease = self._device_manager.acquire_exclusive(
                    device_id=device_id,
                    client_id=client.client_id,
                    description=description,
                    allows_monitors=allows_monitors,
                    timeout=timeout,
                )
            else:  # MONITOR
                lease = self._device_manager.acquire_monitor(
                    device_id=device_id,
                    client_id=client.client_id,
                    description=description,
                )

            if lease:
                logging.info(f"Client {client.client_id} acquired {lease_type.value} lease for device {device_id} (lease_id={lease.lease_id})")

                # Broadcast lease event
                await self.broadcast(
                    SubscriptionType.DEVICES,
                    {
                        "event": "lease_acquired",
                        "client_id": client.client_id,
                        "device_id": device_id,
                        "lease_id": lease.lease_id,
                        "lease_type": lease_type.value,
                    },
                )

                return {
                    "success": True,
                    "message": f"{lease_type.value} lease acquired",
                    "lease_id": lease.lease_id,
                    "device_id": device_id,
                    "lease_type": lease_type.value,
                    "allows_monitors": lease.allows_monitors,
                }
            else:
                device_status = self._device_manager.get_device_status(device_id)
                return {
                    "success": False,
                    "message": "Lease not available",
                    "device_id": device_id,
                    "lease_type": lease_type.value,
                    "is_connected": device_status.get("is_connected", False),
                    "has_exclusive": device_status.get("exclusive_lease") is not None,
                }

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error acquiring device lease for {client.client_id}: {e}")
            return {
                "success": False,
                "message": f"Device lease error: {e}",
            }

    async def _handle_device_release(
        self,
        client: ClientConnection,
        data: dict[str, Any],
    ) -> dict[str, Any]:
        """Handle device lease release request.

        Args:
            client: The client connection
            data: Release request data (lease_id)

        Returns:
            Response with release result
        """
        lease_id = data.get("lease_id", "")

        if not lease_id:
            return {
                "success": False,
                "message": "lease_id is required",
            }

        # Check that device manager is available
        if self._device_manager is None:
            return {
                "success": False,
                "message": "Device manager not available",
            }

        try:
            released = self._device_manager.release_lease(lease_id, client.client_id)

            if released:
                logging.info(f"Client {client.client_id} released lease {lease_id}")

                # Broadcast lease release event
                await self.broadcast(
                    SubscriptionType.DEVICES,
                    {
                        "event": "lease_released",
                        "client_id": client.client_id,
                        "lease_id": lease_id,
                    },
                )

                return {
                    "success": True,
                    "message": "Lease released",
                    "lease_id": lease_id,
                }
            else:
                return {
                    "success": False,
                    "message": "Lease not found or not owned by this client",
                    "lease_id": lease_id,
                }

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error releasing device lease {lease_id} for {client.client_id}: {e}")
            return {
                "success": False,
                "message": f"Device release error: {e}",
            }

    async def _handle_device_preempt(
        self,
        client: ClientConnection,
        data: dict[str, Any],
    ) -> dict[str, Any]:
        """Handle device preemption request.

        Forcibly takes the exclusive lease from the current holder.
        The reason is REQUIRED and must not be empty.

        Args:
            client: The client connection
            data: Preempt request data (device_id, reason)

        Returns:
            Response with preemption result
        """
        device_id = data.get("device_id", "")
        reason = data.get("reason", "")

        if not device_id:
            return {
                "success": False,
                "message": "device_id is required",
            }

        if not reason or not reason.strip():
            return {
                "success": False,
                "message": "reason is required and must not be empty",
            }

        # Check that device manager is available
        if self._device_manager is None:
            return {
                "success": False,
                "message": "Device manager not available",
            }

        try:
            success, preempted_client_id = self._device_manager.preempt_device(
                device_id=device_id,
                requesting_client_id=client.client_id,
                reason=reason,
            )

            if success:
                logging.warning(f"PREEMPTION: {client.client_id} took device {device_id} from {preempted_client_id}. Reason: {reason}")

                # Broadcast preemption event to all subscribers
                await self.broadcast(
                    SubscriptionType.DEVICES,
                    {
                        "event": "device_preempted",
                        "device_id": device_id,
                        "preempted_by": client.client_id,
                        "preempted_client_id": preempted_client_id,
                        "reason": reason,
                    },
                )

                # Send direct notification to preempted client if they're still connected
                if preempted_client_id:
                    preempted_client = await self.get_client_async(preempted_client_id)
                    if preempted_client:
                        await self._send_message(
                            preempted_client,
                            MessageType.BROADCAST,
                            {
                                "event_type": "device_preemption",
                                "data": {
                                    "device_id": device_id,
                                    "preempted_by": client.client_id,
                                    "reason": reason,
                                },
                                "timestamp": time.time(),
                            },
                        )

                # Get the new lease for the requester
                device_status = self._device_manager.get_device_status(device_id)
                new_lease = device_status.get("exclusive_lease")

                return {
                    "success": True,
                    "message": f"Device preempted from {preempted_client_id}",
                    "device_id": device_id,
                    "preempted_client_id": preempted_client_id,
                    "lease_id": new_lease.get("lease_id") if new_lease else None,
                    "lease_type": "exclusive",
                }
            else:
                return {
                    "success": False,
                    "message": "Preemption failed - device may not have an exclusive holder",
                    "device_id": device_id,
                }

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error preempting device {device_id} for {client.client_id}: {e}")
            return {
                "success": False,
                "message": f"Device preemption error: {e}",
            }

    async def _handle_device_status(
        self,
        client: ClientConnection,  # noqa: ARG002
        data: dict[str, Any],
    ) -> dict[str, Any]:
        """Handle device status request.

        Args:
            client: The client connection (unused but required for handler signature)
            data: Status request data (device_id)

        Returns:
            Response with device status
        """
        device_id = data.get("device_id", "")

        if not device_id:
            return {
                "success": False,
                "message": "device_id is required",
            }

        # Check that device manager is available
        if self._device_manager is None:
            return {
                "success": False,
                "message": "Device manager not available",
                "device_id": device_id,
                "exists": False,
            }

        try:
            status = self._device_manager.get_device_status(device_id)

            if not status.get("exists", False):
                return {
                    "success": True,
                    "message": "Device not found",
                    "device_id": device_id,
                    "exists": False,
                    "is_connected": False,
                }

            return {
                "success": True,
                "message": "Device status retrieved",
                **status,
            }

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error getting device status for {device_id}: {e}")
            return {
                "success": False,
                "message": f"Device status error: {e}",
                "device_id": device_id,
            }

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
