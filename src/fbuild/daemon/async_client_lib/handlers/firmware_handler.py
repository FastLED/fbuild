"""
Firmware protocol handler for async daemon client.

This module handles firmware query operations and firmware change subscriptions.
"""

from __future__ import annotations

from typing import Any

from ..types import MessageType
from .base import BaseProtocolHandler


class FirmwareProtocolHandler(BaseProtocolHandler):
    """Handles firmware-related protocol operations.

    This handler provides methods for querying firmware status on devices.
    """

    async def query(
        self,
        port: str,
        source_hash: str,
        build_flags_hash: str | None = None,
    ) -> dict[str, Any]:
        """Query if firmware is current on a device.

        Args:
            port: Serial port of the device
            source_hash: Hash of the source files
            build_flags_hash: Hash of build flags (optional)

        Returns:
            Dictionary with firmware status:
            - is_current: True if firmware matches
            - needs_redeploy: True if source changed
            - firmware_hash: Hash of deployed firmware
            - project_dir: Project directory of deployed firmware
            - environment: Environment of deployed firmware
            - upload_timestamp: When firmware was last uploaded
        """
        return await self.send_request(
            MessageType.FIRMWARE_QUERY,
            {
                "port": port,
                "source_hash": source_hash,
                "build_flags_hash": build_flags_hash,
            },
        )
