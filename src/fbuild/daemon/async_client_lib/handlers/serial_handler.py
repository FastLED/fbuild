"""
Serial protocol handler for async daemon client.

This module handles serial session operations (attach, detach, read, write).
"""

from __future__ import annotations

import base64

from ..types import MessageType
from .base import BaseProtocolHandler


class SerialProtocolHandler(BaseProtocolHandler):
    """Handles serial session protocol operations.

    This handler provides methods for managing serial port connections,
    reading/writing data, and controlling write access.
    """

    async def attach(
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
        response = await self.send_request(
            MessageType.SERIAL_ATTACH,
            {
                "port": port,
                "baud_rate": baud_rate,
                "as_reader": as_reader,
            },
        )
        return response.get("success", False)

    async def detach(
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
        response = await self.send_request(
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
        response = await self.send_request(
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
        response = await self.send_request(
            MessageType.SERIAL_RELEASE_WRITER,
            {"port": port},
        )
        return response.get("success", False)

    async def write(
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

        response = await self.send_request(
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
        response = await self.send_request(
            MessageType.SERIAL_READ_BUFFER,
            {
                "port": port,
                "max_lines": max_lines,
            },
        )

        if not response.get("success", False):
            return []

        return response.get("lines", [])
