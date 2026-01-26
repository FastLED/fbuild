"""
Message handler registry for the async daemon server.

This module provides a factory function to create a handler registry
mapping MessageType to handler instances.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any, Callable, Coroutine

if TYPE_CHECKING:
    from fbuild.daemon.async_server import ClientConnection, MessageType

from fbuild.daemon.handlers.base_handler import HandlerContext, MessageHandler
from fbuild.daemon.handlers.client_handlers import (
    ClientConnectHandler,
    ClientDisconnectHandler,
    ClientHeartbeatHandler,
)
from fbuild.daemon.handlers.device_handlers import (
    DeviceLeaseHandler,
    DeviceListHandler,
    DevicePreemptHandler,
    DeviceReleaseHandler,
    DeviceStatusHandler,
)
from fbuild.daemon.handlers.firmware_handlers import (
    FirmwareQueryHandler,
    FirmwareRecordHandler,
)
from fbuild.daemon.handlers.lock_handlers import (
    LockAcquireHandler,
    LockReleaseHandler,
    LockStatusHandler,
)
from fbuild.daemon.handlers.serial_handlers import (
    SerialAttachHandler,
    SerialDetachHandler,
    SerialReadHandler,
    SerialWriteHandler,
)
from fbuild.daemon.handlers.subscription_handlers import (
    SubscribeHandler,
    UnsubscribeHandler,
)

__all__ = [
    "HandlerContext",
    "MessageHandler",
    "create_handler_registry",
    # Lock handlers
    "LockAcquireHandler",
    "LockReleaseHandler",
    "LockStatusHandler",
    # Firmware handlers
    "FirmwareQueryHandler",
    "FirmwareRecordHandler",
    # Serial handlers
    "SerialAttachHandler",
    "SerialDetachHandler",
    "SerialWriteHandler",
    "SerialReadHandler",
    # Device handlers
    "DeviceListHandler",
    "DeviceLeaseHandler",
    "DeviceReleaseHandler",
    "DevicePreemptHandler",
    "DeviceStatusHandler",
    # Subscription handlers
    "SubscribeHandler",
    "UnsubscribeHandler",
    # Client handlers
    "ClientConnectHandler",
    "ClientHeartbeatHandler",
    "ClientDisconnectHandler",
]


def create_handler_registry(
    context: HandlerContext,
    clients: dict[str, "ClientConnection"],
    get_client_async: Callable[[str], Coroutine[Any, Any, "ClientConnection | None"]],
) -> dict["MessageType", MessageHandler]:
    """Create a handler registry mapping MessageType to handler instances.

    Args:
        context: Handler context with daemon managers and utility functions
        clients: Dictionary of connected clients (for ClientConnectHandler)
        get_client_async: Async function to get client by ID (for DevicePreemptHandler)

    Returns:
        Dictionary mapping MessageType to handler instances
    """
    # Import MessageType here to avoid circular dependency
    from fbuild.daemon.async_server import MessageType

    return {
        # Client lifecycle
        MessageType.CONNECT: ClientConnectHandler(context, clients),
        MessageType.HEARTBEAT: ClientHeartbeatHandler(context),
        MessageType.DISCONNECT: ClientDisconnectHandler(context),
        # Lock operations
        MessageType.LOCK_ACQUIRE: LockAcquireHandler(context),
        MessageType.LOCK_RELEASE: LockReleaseHandler(context),
        MessageType.LOCK_STATUS: LockStatusHandler(context),
        # Firmware operations
        MessageType.FIRMWARE_QUERY: FirmwareQueryHandler(context),
        MessageType.FIRMWARE_RECORD: FirmwareRecordHandler(context),
        # Serial operations
        MessageType.SERIAL_ATTACH: SerialAttachHandler(context),
        MessageType.SERIAL_DETACH: SerialDetachHandler(context),
        MessageType.SERIAL_WRITE: SerialWriteHandler(context),
        MessageType.SERIAL_READ: SerialReadHandler(context),
        # Device operations
        MessageType.DEVICE_LIST: DeviceListHandler(context),
        MessageType.DEVICE_LEASE: DeviceLeaseHandler(context),
        MessageType.DEVICE_RELEASE: DeviceReleaseHandler(context),
        MessageType.DEVICE_PREEMPT: DevicePreemptHandler(context, get_client_async),
        MessageType.DEVICE_STATUS: DeviceStatusHandler(context),
        # Subscription
        MessageType.SUBSCRIBE: SubscribeHandler(context),
        MessageType.UNSUBSCRIBE: UnsubscribeHandler(context),
    }
