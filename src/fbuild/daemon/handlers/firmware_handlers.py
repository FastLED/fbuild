"""
Firmware operation handlers for the async daemon server.

Handles firmware query and record operations.
"""

from __future__ import annotations

import logging
from typing import TYPE_CHECKING, Any

from fbuild.daemon.handlers.base_handler import HandlerContext

if TYPE_CHECKING:
    from fbuild.daemon.async_server import ClientConnection

# Import SubscriptionType for broadcast
from fbuild.daemon.async_server import SubscriptionType


class FirmwareQueryHandler:
    """Handler for firmware query requests."""

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
        """Handle firmware query request.

        Args:
            client: The client connection (unused but required for handler signature)
            data: Query data (port, source_hash, build_flags_hash)

        Returns:
            Response with firmware status
        """
        port = data.get("port", "")
        source_hash = data.get("source_hash", "")
        build_flags_hash = data.get("build_flags_hash")

        # Check that firmware ledger is available
        if self.context.firmware_ledger is None:
            return {
                "success": False,
                "is_current": False,
                "needs_redeploy": True,
                "message": "Firmware ledger not available",
            }

        try:
            entry = self.context.firmware_ledger.get_deployment(port)

            if entry is None:
                return {
                    "success": True,
                    "is_current": False,
                    "needs_redeploy": True,
                    "message": "No firmware deployment recorded for this port",
                }

            is_current = entry.source_hash == source_hash
            if build_flags_hash and entry.build_flags_hash != build_flags_hash:
                is_current = False

            return {
                "success": True,
                "is_current": is_current,
                "needs_redeploy": not is_current,
                "firmware_hash": entry.firmware_hash,
                "project_dir": entry.project_dir,
                "environment": entry.environment,
                "upload_timestamp": entry.upload_timestamp,
                "message": "Firmware current" if is_current else "Firmware needs update",
            }

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error querying firmware: {e}")
            return {
                "success": False,
                "is_current": False,
                "needs_redeploy": True,
                "message": f"Firmware query error: {e}",
            }


class FirmwareRecordHandler:
    """Handler for firmware record requests."""

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
        """Handle firmware record request.

        Args:
            client: The client connection
            data: Record data (port, firmware_hash, source_hash, project_dir, environment)

        Returns:
            Response confirming record creation
        """
        port = data.get("port", "")
        firmware_hash = data.get("firmware_hash", "")
        source_hash = data.get("source_hash", "")
        project_dir = data.get("project_dir", "")
        environment = data.get("environment", "")
        build_flags_hash = data.get("build_flags_hash")

        # Check that firmware ledger is available
        if self.context.firmware_ledger is None:
            return {
                "success": False,
                "message": "Firmware ledger not available",
            }

        try:
            self.context.firmware_ledger.record_deployment(
                port=port,
                firmware_hash=firmware_hash,
                source_hash=source_hash,
                project_dir=project_dir,
                environment=environment,
                build_flags_hash=build_flags_hash,
            )

            logging.info(f"Recorded firmware deployment to {port} by client {client.client_id}")

            # Broadcast firmware event
            await self.context.broadcast(
                SubscriptionType.FIRMWARE,
                {
                    "event": "firmware_deployed",
                    "port": port,
                    "project_dir": project_dir,
                    "environment": environment,
                    "client_id": client.client_id,
                },
                None,  # exclude_client_id
            )

            return {
                "success": True,
                "message": "Firmware deployment recorded",
            }

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error recording firmware: {e}")
            return {
                "success": False,
                "message": f"Firmware record error: {e}",
            }
