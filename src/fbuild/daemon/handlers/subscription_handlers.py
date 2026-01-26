"""
Subscription operation handlers for the async daemon server.

Handles client subscribe and unsubscribe requests.
"""

from __future__ import annotations

import logging
from typing import TYPE_CHECKING, Any

from fbuild.daemon.handlers.base_handler import HandlerContext

if TYPE_CHECKING:
    from fbuild.daemon.async_server import ClientConnection

# Import SubscriptionType for subscription management
from fbuild.daemon.async_server import SubscriptionType


class SubscribeHandler:
    """Handler for subscription requests."""

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


class UnsubscribeHandler:
    """Handler for unsubscription requests."""

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
