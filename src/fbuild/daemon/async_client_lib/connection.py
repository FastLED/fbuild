"""
Connection management for async daemon client.

This module handles connection state, reconnection logic, and background tasks.
"""

from __future__ import annotations

import asyncio
import json
import logging
import os
import socket
import time
from typing import Any

from .types import ConnectionError, ConnectionState, MessageType

# Default configuration
DEFAULT_RECONNECT_DELAY = 1.0
DEFAULT_MAX_RECONNECT_DELAY = 60.0
DEFAULT_RECONNECT_BACKOFF_FACTOR = 2.0


class ConnectionManager:
    """Manages connection lifecycle and background tasks.

    This class encapsulates connection state, stream I/O, and reconnection logic,
    keeping the main client class focused on protocol operations.
    """

    def __init__(
        self,
        client_id: str,
        auto_reconnect: bool = True,
        reconnect_delay: float = DEFAULT_RECONNECT_DELAY,
        max_reconnect_delay: float = DEFAULT_MAX_RECONNECT_DELAY,
        reconnect_backoff_factor: float = DEFAULT_RECONNECT_BACKOFF_FACTOR,
        logger: logging.Logger | None = None,
    ) -> None:
        """Initialize the connection manager.

        Args:
            client_id: Client identifier
            auto_reconnect: Whether to automatically reconnect on disconnect
            reconnect_delay: Initial delay before reconnecting in seconds
            max_reconnect_delay: Maximum delay between reconnect attempts
            reconnect_backoff_factor: Factor to multiply delay on each retry
            logger: Logger instance (creates one if None)
        """
        self._client_id = client_id
        self._auto_reconnect = auto_reconnect
        self._reconnect_delay = reconnect_delay
        self._max_reconnect_delay = max_reconnect_delay
        self._reconnect_backoff_factor = reconnect_backoff_factor
        self._logger = logger or logging.getLogger(f"ConnectionManager[{client_id[:8]}]")

        # Connection state
        self._state = ConnectionState.DISCONNECTED
        self._host: str | None = None
        self._port: int | None = None
        self._reader: asyncio.StreamReader | None = None
        self._writer: asyncio.StreamWriter | None = None

        # Shutdown flag
        self._shutdown_requested = False

    @property
    def state(self) -> ConnectionState:
        """Get the current connection state."""
        return self._state

    @property
    def is_connected(self) -> bool:
        """Check if connected."""
        return self._state == ConnectionState.CONNECTED

    @property
    def reader(self) -> asyncio.StreamReader | None:
        """Get the stream reader."""
        return self._reader

    @property
    def writer(self) -> asyncio.StreamWriter | None:
        """Get the stream writer."""
        return self._writer

    async def connect(
        self,
        host: str,
        port: int,
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
        self._host = host
        self._port = port
        self._state = ConnectionState.CONNECTING
        self._shutdown_requested = False

        try:
            self._logger.info(f"Connecting to daemon at {host}:{port}")

            # Open connection with timeout
            self._reader, self._writer = await asyncio.wait_for(
                asyncio.open_connection(host, port),
                timeout=timeout,
            )

            # Send client connect message
            await self._send_client_connect()

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

        self._logger.info("Disconnected from daemon")

    async def send_message(self, message: dict[str, Any]) -> None:
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

    async def _send_client_connect(self) -> None:
        """Send client connect message."""
        await self.send_message(
            {
                "type": MessageType.CLIENT_CONNECT.value,
                "pid": os.getpid(),
                "hostname": socket.gethostname(),
                "version": "1.0.0",  # TODO: Get from package version
            }
        )

    async def _send_client_disconnect(self, reason: str) -> None:
        """Send client disconnect message."""
        await self.send_message(
            {
                "type": MessageType.CLIENT_DISCONNECT.value,
                "reason": reason,
            }
        )

    async def send_heartbeat(self) -> None:
        """Send heartbeat message."""
        try:
            await self.send_message({"type": MessageType.CLIENT_HEARTBEAT.value})
        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            self._logger.warning(f"Failed to send heartbeat: {e}")

    async def reconnect(self, connect_callback: Any) -> None:
        """Attempt to reconnect to the daemon.

        Args:
            connect_callback: Callback to call after successful reconnection
        """
        if not self._host or not self._port:
            self._logger.error("Cannot reconnect: no host/port configured")
            return

        self._state = ConnectionState.RECONNECTING
        delay = self._reconnect_delay

        while not self._shutdown_requested and self._auto_reconnect:
            self._logger.info(f"Attempting reconnect in {delay}s")
            await asyncio.sleep(delay)

            try:
                await connect_callback(self._host, self._port)
                self._logger.info("Reconnection successful")
                return

            except ConnectionError as e:
                self._logger.warning(f"Reconnection failed: {e}")
                delay = min(delay * self._reconnect_backoff_factor, self._max_reconnect_delay)

        self._state = ConnectionState.DISCONNECTED

    @property
    def shutdown_requested(self) -> bool:
        """Check if shutdown was requested."""
        return self._shutdown_requested

    @property
    def auto_reconnect(self) -> bool:
        """Check if auto-reconnect is enabled."""
        return self._auto_reconnect
