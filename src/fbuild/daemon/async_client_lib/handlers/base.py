"""
Base protocol handler for async daemon client.

This module defines the base protocol handler interface that all
protocol-specific handlers inherit from.
"""

from __future__ import annotations

import logging
from typing import TYPE_CHECKING, Any, Callable, Coroutine

if TYPE_CHECKING:
    from ..types import MessageType


class BaseProtocolHandler:
    """Base class for protocol handlers.

    Protocol handlers encapsulate domain-specific operations (locks, firmware, serial, etc.)
    and delegate actual message sending to a callback provided by the client.

    This pattern allows handlers to be independent of the client's connection logic
    while still being able to send requests to the daemon.
    """

    def __init__(
        self,
        send_request: Callable[[MessageType, dict[str, Any], float | None], Coroutine[Any, Any, dict[str, Any]]],
        logger: logging.Logger | None = None,
    ) -> None:
        """Initialize the protocol handler.

        Args:
            send_request: Callback to send requests to the daemon.
                         Signature: async (message_type, payload, timeout) -> response
            logger: Logger instance (creates one if None)
        """
        self._send_request = send_request
        self._logger = logger or logging.getLogger(self.__class__.__name__)

    async def send_request(
        self,
        message_type: MessageType,
        payload: dict[str, Any],
        timeout: float | None = None,
    ) -> dict[str, Any]:
        """Send a request to the daemon.

        Args:
            message_type: Type of request
            payload: Request payload
            timeout: Request timeout (uses default if None)

        Returns:
            Response dictionary
        """
        return await self._send_request(message_type, payload, timeout)
