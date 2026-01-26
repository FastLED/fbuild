"""
Lock operation handlers for the async daemon server.

Handles configuration lock acquire, release, and status queries.
"""

from __future__ import annotations

import logging
from typing import TYPE_CHECKING, Any

from fbuild.daemon.handlers.base_handler import HandlerContext

if TYPE_CHECKING:
    from fbuild.daemon.async_server import ClientConnection, SubscriptionType

# Import SubscriptionType for broadcast
from fbuild.daemon.async_server import SubscriptionType


class LockAcquireHandler:
    """Handler for lock acquire requests."""

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
        """Handle lock acquire request.

        Args:
            client: The client connection
            data: Lock request data (project_dir, environment, port, lock_type, etc.)

        Returns:
            Response with lock acquisition result
        """
        from fbuild.daemon.messages import LockType

        project_dir = data.get("project_dir", "")
        environment = data.get("environment", "")
        port = data.get("port", "")
        lock_type_str = data.get("lock_type", "exclusive")
        description = data.get("description", "")
        timeout = data.get("timeout", 300.0)

        config_key = (project_dir, environment, port)

        try:
            lock_type = LockType(lock_type_str)
        except ValueError:
            return {
                "success": False,
                "message": f"Invalid lock type: {lock_type_str}",
            }

        # Check that configuration lock manager is available
        if self.context.configuration_lock_manager is None:
            return {
                "success": False,
                "message": "Lock manager not available",
            }

        # Acquire lock (thread-safe through ConfigurationLockManager)
        try:
            if lock_type == LockType.EXCLUSIVE:
                acquired = self.context.configuration_lock_manager.acquire_exclusive(
                    config_key,
                    client.client_id,
                    description,
                    timeout,
                )
            else:  # SHARED_READ
                acquired = self.context.configuration_lock_manager.acquire_shared_read(
                    config_key,
                    client.client_id,
                    description,
                )

            if acquired:
                logging.info(f"Client {client.client_id} acquired {lock_type.value} lock for {config_key}")

                # Broadcast lock change
                await self.context.broadcast(
                    SubscriptionType.LOCKS,
                    {
                        "event": "lock_acquired",
                        "client_id": client.client_id,
                        "config_key": {"project_dir": project_dir, "environment": environment, "port": port},
                        "lock_type": lock_type.value,
                    },
                    None,  # exclude_client_id
                )

                return {
                    "success": True,
                    "message": f"{lock_type.value} lock acquired",
                    "lock_state": f"locked_{lock_type.value}",
                }
            else:
                lock_status = self.context.configuration_lock_manager.get_lock_status(config_key)
                return {
                    "success": False,
                    "message": "Lock not available",
                    "lock_state": lock_status.get("state", "unknown"),
                    "holder_count": lock_status.get("holder_count", 0),
                    "waiting_count": lock_status.get("waiting_count", 0),
                }

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error acquiring lock for {client.client_id}: {e}")
            return {
                "success": False,
                "message": f"Lock acquisition error: {e}",
            }


class LockReleaseHandler:
    """Handler for lock release requests."""

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
        """Handle lock release request.

        Args:
            client: The client connection
            data: Lock release data (project_dir, environment, port)

        Returns:
            Response with lock release result
        """
        project_dir = data.get("project_dir", "")
        environment = data.get("environment", "")
        port = data.get("port", "")

        config_key = (project_dir, environment, port)

        # Check that configuration lock manager is available
        if self.context.configuration_lock_manager is None:
            return {
                "success": False,
                "message": "Lock manager not available",
            }

        try:
            released = self.context.configuration_lock_manager.release(
                config_key,
                client.client_id,
            )

            if released:
                logging.info(f"Client {client.client_id} released lock for {config_key}")

                # Broadcast lock change
                await self.context.broadcast(
                    SubscriptionType.LOCKS,
                    {
                        "event": "lock_released",
                        "client_id": client.client_id,
                        "config_key": {"project_dir": project_dir, "environment": environment, "port": port},
                    },
                    None,  # exclude_client_id
                )

                return {
                    "success": True,
                    "message": "Lock released",
                    "lock_state": "unlocked",
                }
            else:
                return {
                    "success": False,
                    "message": "Client does not hold this lock",
                }

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error releasing lock for {client.client_id}: {e}")
            return {
                "success": False,
                "message": f"Lock release error: {e}",
            }


class LockStatusHandler:
    """Handler for lock status queries."""

    def __init__(self, context: HandlerContext) -> None:
        """Initialize handler with context.

        Args:
            context: Handler context with dependencies
        """
        self.context = context

    async def handle(
        self,
        client: "ClientConnection",  # noqa: ARG002
        data: dict[str, Any],
    ) -> dict[str, Any]:
        """Handle lock status query.

        Args:
            client: The client connection (unused but required for handler signature)
            data: Lock query data (project_dir, environment, port)

        Returns:
            Response with current lock status
        """
        project_dir = data.get("project_dir", "")
        environment = data.get("environment", "")
        port = data.get("port", "")

        config_key = (project_dir, environment, port)

        # Check that configuration lock manager is available
        if self.context.configuration_lock_manager is None:
            return {
                "success": False,
                "message": "Lock manager not available",
            }

        try:
            lock_status = self.context.configuration_lock_manager.get_lock_status(config_key)
            return {
                "success": True,
                **lock_status,
            }

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error getting lock status: {e}")
            return {
                "success": False,
                "message": f"Lock status error: {e}",
            }
