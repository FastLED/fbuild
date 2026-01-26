"""
Async Client Library for fbuild daemon.

This package provides asynchronous and synchronous clients for connecting to the fbuild
daemon's async server. It supports:

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

# Convenience function to create a client
from typing import Any

from .client import (
    DEFAULT_HEARTBEAT_INTERVAL,
    DEFAULT_HOST,
    DEFAULT_PORT,
    DEFAULT_REQUEST_TIMEOUT,
    AsyncDaemonClient,
)
from .sync_client import SyncDaemonClient
from .types import (
    ConnectionError,
    ConnectionState,
    DaemonClientError,
    MessageType,
    PendingRequest,
    ProtocolError,
    Subscription,
    TimeoutError,
)


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


__all__ = [
    # Clients
    "AsyncDaemonClient",
    "SyncDaemonClient",
    "create_client",
    # Types
    "ConnectionState",
    "MessageType",
    "PendingRequest",
    "Subscription",
    # Exceptions
    "DaemonClientError",
    "ConnectionError",
    "TimeoutError",
    "ProtocolError",
    # Constants
    "DEFAULT_HOST",
    "DEFAULT_PORT",
    "DEFAULT_REQUEST_TIMEOUT",
    "DEFAULT_HEARTBEAT_INTERVAL",
]
