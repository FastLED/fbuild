"""
Client lifecycle handlers for the async daemon server.

Handles client connect, heartbeat, and disconnect operations.
"""

from __future__ import annotations

import asyncio
import logging
from typing import TYPE_CHECKING, Any

from fbuild.daemon.handlers.base_handler import HandlerContext

if TYPE_CHECKING:
    from fbuild.daemon.async_server import ClientConnection

# Import SubscriptionType for broadcast
from fbuild.daemon.async_server import SubscriptionType


class ClientConnectHandler:
    """Handler for client connect requests."""

    def __init__(self, context: HandlerContext, clients: dict[str, "ClientConnection"]) -> None:
        """Initialize handler with context.

        Args:
            context: Handler context with dependencies
            clients: Dictionary of connected clients (for total count)
        """
        self.context = context
        self.clients = clients

    async def handle(
        self,
        client: "ClientConnection",
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
        if self.context.client_manager is not None:
            try:
                self.context.client_manager.register_client(
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
        await self.context.broadcast(
            SubscriptionType.STATUS,
            {
                "event": "client_connected",
                "client_id": client.client_id,
                "metadata": client.metadata,
            },
            client.client_id,  # exclude_client_id
        )

        return {
            "success": True,
            "client_id": client.client_id,
            "message": "Connected successfully",
            "total_clients": len(self.clients),
        }


class ClientHeartbeatHandler:
    """Handler for client heartbeat requests."""

    def __init__(self, context: HandlerContext) -> None:
        """Initialize handler with context.

        Args:
            context: Handler context with dependencies
        """
        self.context = context

    async def handle(
        self,
        client: "ClientConnection",
        data: dict[str, Any],  # noqa: ARG002
    ) -> dict[str, Any]:
        """Handle client heartbeat message.

        Args:
            client: The client connection
            data: Heartbeat data (unused but required for handler signature)

        Returns:
            Response acknowledging the heartbeat
        """
        import time

        client.update_heartbeat()

        # Update in DaemonContext client manager
        if self.context.client_manager is not None:
            self.context.client_manager.heartbeat(client.client_id)

        logging.debug(f"Heartbeat from client {client.client_id}")

        return {
            "success": True,
            "message": "Heartbeat acknowledged",
            "timestamp": time.time(),
        }


class ClientDisconnectHandler:
    """Handler for client disconnect requests."""

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
        asyncio.create_task(self.context.disconnect_client(client.client_id, reason))

        return {
            "success": True,
            "message": "Disconnect acknowledged",
        }
