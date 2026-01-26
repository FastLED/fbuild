"""
Base handler protocol and context for async daemon message handlers.

This module defines the core protocol and dependency injection context
used by all message handlers in the async daemon server.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, Callable, Coroutine, Protocol, runtime_checkable

if TYPE_CHECKING:
    from fbuild.daemon.async_client import ClientConnectionManager
    from fbuild.daemon.async_server import ClientConnection, SubscriptionType
    from fbuild.daemon.configuration_lock import ConfigurationLockManager
    from fbuild.daemon.device_manager import DeviceManager
    from fbuild.daemon.firmware_ledger import FirmwareLedger
    from fbuild.daemon.shared_serial import SharedSerialManager


@dataclass
class HandlerContext:
    """Dependency injection context for message handlers.

    Provides access to daemon managers and utility functions needed by handlers.

    Attributes:
        configuration_lock_manager: Manager for configuration locks
        firmware_ledger: Ledger for firmware deployment tracking
        shared_serial_manager: Manager for shared serial port access
        client_manager: Manager for client connection tracking
        device_manager: Manager for device leasing
        broadcast: Function to broadcast messages to subscribed clients
        disconnect_client: Function to disconnect a client
    """

    configuration_lock_manager: "ConfigurationLockManager | None"
    firmware_ledger: "FirmwareLedger | None"
    shared_serial_manager: "SharedSerialManager | None"
    client_manager: "ClientConnectionManager | None"
    device_manager: "DeviceManager | None"
    broadcast: Callable[["SubscriptionType", dict[str, Any], str | None], Coroutine[Any, Any, int]]
    disconnect_client: Callable[[str, str], Coroutine[Any, Any, None]]


@runtime_checkable
class MessageHandler(Protocol):
    """Protocol for async message handlers.

    All message handlers must implement this protocol, accepting a client
    connection and message data, and returning a response dictionary.

    This follows the same pattern as RequestProcessor in the daemon,
    but adapted for async operation.
    """

    async def handle(
        self,
        client: "ClientConnection",
        data: dict[str, Any],
    ) -> dict[str, Any]:
        """Handle a message from a client.

        Args:
            client: The client connection that sent the message
            data: Message data dictionary

        Returns:
            Response dictionary to send back to the client
        """
        ...
