"""
Subscription handler for async daemon client.

This module handles subscription management and event broadcasting.
"""

from __future__ import annotations

import asyncio
import uuid
from typing import Any, Callable, Coroutine

from ..types import MessageType, Subscription
from .base import BaseProtocolHandler


class SubscriptionProtocolHandler(BaseProtocolHandler):
    """Handles event subscriptions and broadcasts.

    This handler manages subscription lifecycle and dispatches broadcast
    events to registered callbacks.
    """

    def __init__(self, *args: Any, **kwargs: Any) -> None:
        """Initialize the subscription handler."""
        super().__init__(*args, **kwargs)
        self._subscriptions: dict[str, Subscription] = {}

    def register_subscription(
        self,
        subscription_id: str,
        event_type: str,
        callback: Callable[[dict[str, Any]], None] | Callable[[dict[str, Any]], Coroutine[Any, Any, None]],
        filter_key: str | None = None,
    ) -> None:
        """Register a subscription.

        Args:
            subscription_id: Unique subscription ID
            event_type: Type of events to subscribe to
            callback: Function to call when event is received
            filter_key: Optional key to filter events
        """
        subscription = Subscription(
            subscription_id=subscription_id,
            event_type=event_type,
            callback=callback,
            filter_key=filter_key,
        )
        self._subscriptions[subscription_id] = subscription

    def unregister_subscription(self, subscription_id: str) -> bool:
        """Unregister a subscription.

        Args:
            subscription_id: Subscription ID to remove

        Returns:
            True if subscription existed and was removed
        """
        if subscription_id in self._subscriptions:
            del self._subscriptions[subscription_id]
            return True
        return False

    def get_subscription(self, subscription_id: str) -> Subscription | None:
        """Get a subscription by ID.

        Args:
            subscription_id: Subscription ID

        Returns:
            Subscription object or None if not found
        """
        return self._subscriptions.get(subscription_id)

    async def handle_broadcast(self, message: dict[str, Any]) -> None:
        """Handle a broadcast event by dispatching to matching subscriptions.

        Args:
            message: Broadcast message containing event_type and filter_key
        """
        event_type = message.get("event_type", "")
        filter_key = message.get("filter_key")

        for subscription in self._subscriptions.values():
            if subscription.event_type == event_type:
                # Check filter
                if subscription.filter_key is not None and subscription.filter_key != filter_key:
                    continue

                # Call callback
                try:
                    result = subscription.callback(message)
                    if asyncio.iscoroutine(result):
                        await result
                except KeyboardInterrupt:  # noqa: KBI002
                    raise
                except Exception as e:
                    self._logger.error(f"Error in subscription callback: {e}")

    def generate_subscription_id(self) -> str:
        """Generate a unique subscription ID.

        Returns:
            UUID-based subscription ID
        """
        return str(uuid.uuid4())

    def clear_subscriptions(self) -> None:
        """Clear all subscriptions."""
        self._subscriptions.clear()

    def has_subscription(self, subscription_id: str) -> bool:
        """Check if a subscription exists.

        Args:
            subscription_id: Subscription ID to check

        Returns:
            True if subscription exists
        """
        return subscription_id in self._subscriptions

    async def subscribe(
        self,
        message_type: MessageType,
        event_type: str,
        callback: Callable[[dict[str, Any]], None] | Callable[[dict[str, Any]], Coroutine[Any, Any, None]],
        filter_key: str | None = None,
        **extra_payload: Any,
    ) -> str:
        """Subscribe to an event type.

        Args:
            message_type: Subscribe message type
            event_type: Event type to subscribe to
            callback: Callback function
            filter_key: Optional filter key
            **extra_payload: Additional payload fields

        Returns:
            Subscription ID
        """
        subscription_id = self.generate_subscription_id()
        self.register_subscription(subscription_id, event_type, callback, filter_key)

        await self.send_request(
            message_type,
            {
                "subscription_id": subscription_id,
                "filter_key": filter_key,
                **extra_payload,
            },
        )

        return subscription_id

    async def unsubscribe(
        self,
        message_type: MessageType,
        subscription_id: str,
    ) -> bool:
        """Unsubscribe from an event.

        Args:
            message_type: Unsubscribe message type
            subscription_id: Subscription ID

        Returns:
            True if unsubscribed successfully
        """
        if not self.has_subscription(subscription_id):
            return False

        await self.send_request(
            message_type,
            {"subscription_id": subscription_id},
        )

        self.unregister_subscription(subscription_id)
        return True
