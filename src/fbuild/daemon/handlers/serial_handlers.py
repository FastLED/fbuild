"""
Serial port operation handlers for the async daemon server.

Handles serial attach, detach, write, and read operations.
"""

from __future__ import annotations

import base64
import logging
from typing import TYPE_CHECKING, Any

from fbuild.daemon.handlers.base_handler import HandlerContext

if TYPE_CHECKING:
    from fbuild.daemon.async_server import ClientConnection

# Import SubscriptionType for broadcast
from fbuild.daemon.async_server import SubscriptionType


class SerialAttachHandler:
    """Handler for serial port attach requests."""

    def __init__(self, context: HandlerContext) -> None:
        """Initialize handler with context.

        Args:
            context: Handler context with dependencies
        """
        self.context = context

    async def handle(
        self,
        client: "ClientConnection",
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
        if self.context.shared_serial_manager is None:
            return {
                "success": False,
                "message": "Serial manager not available",
            }

        try:
            # Open port if not already open
            opened = self.context.shared_serial_manager.open_port(
                port,
                baud_rate,
                client.client_id,
            )

            if as_reader:
                attached = self.context.shared_serial_manager.attach_reader(
                    port,
                    client.client_id,
                )
            else:
                attached = opened

            if attached:
                session_info = self.context.shared_serial_manager.get_session_info(port)

                # Broadcast serial event
                await self.context.broadcast(
                    SubscriptionType.SERIAL,
                    {
                        "event": "client_attached",
                        "port": port,
                        "client_id": client.client_id,
                        "as_reader": as_reader,
                    },
                    None,  # exclude_client_id
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


class SerialDetachHandler:
    """Handler for serial port detach requests."""

    def __init__(self, context: HandlerContext) -> None:
        """Initialize handler with context.

        Args:
            context: Handler context with dependencies
        """
        self.context = context

    async def handle(
        self,
        client: "ClientConnection",
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
        if self.context.shared_serial_manager is None:
            return {
                "success": False,
                "message": "Serial manager not available",
            }

        try:
            detached = self.context.shared_serial_manager.detach_reader(
                port,
                client.client_id,
            )

            if close_port:
                self.context.shared_serial_manager.close_port(port, client.client_id)

            if detached:
                # Broadcast serial event
                await self.context.broadcast(
                    SubscriptionType.SERIAL,
                    {
                        "event": "client_detached",
                        "port": port,
                        "client_id": client.client_id,
                    },
                    None,  # exclude_client_id
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


class SerialWriteHandler:
    """Handler for serial port write requests."""

    def __init__(self, context: HandlerContext) -> None:
        """Initialize handler with context.

        Args:
            context: Handler context with dependencies
        """
        self.context = context

    async def handle(
        self,
        client: "ClientConnection",
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
        if self.context.shared_serial_manager is None:
            return {
                "success": False,
                "message": "Serial manager not available",
            }

        try:
            # Decode base64 data
            write_data = base64.b64decode(data_b64)

            # Acquire writer if needed
            if acquire_writer:
                acquired = self.context.shared_serial_manager.acquire_writer(
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
            bytes_written = self.context.shared_serial_manager.write(
                port,
                client.client_id,
                write_data,
            )

            # Release writer if we acquired it
            if acquire_writer:
                self.context.shared_serial_manager.release_writer(port, client.client_id)

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


class SerialReadHandler:
    """Handler for serial port read (buffer) requests."""

    def __init__(self, context: HandlerContext) -> None:
        """Initialize handler with context.

        Args:
            context: Handler context with dependencies
        """
        self.context = context

    async def handle(
        self,
        client: "ClientConnection",
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
        if self.context.shared_serial_manager is None:
            return {
                "success": False,
                "message": "Serial manager not available",
                "lines": [],
            }

        try:
            lines = self.context.shared_serial_manager.read_buffer(
                port,
                client.client_id,
                max_lines,
            )

            session_info = self.context.shared_serial_manager.get_session_info(port)

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
