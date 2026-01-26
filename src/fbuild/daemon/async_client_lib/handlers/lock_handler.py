"""
Lock protocol handler for async daemon client.

This module handles configuration lock operations (acquire, release, status, subscriptions).
"""

from __future__ import annotations

from typing import Any

from ..types import MessageType
from .base import BaseProtocolHandler


class LockProtocolHandler(BaseProtocolHandler):
    """Handles lock-related protocol operations.

    This handler provides methods for acquiring, releasing, and querying
    configuration locks, as well as subscribing to lock change events.
    """

    async def acquire(
        self,
        project_dir: str,
        environment: str,
        port: str,
        lock_type: str = "exclusive",
        timeout: float = 300.0,
        description: str = "",
    ) -> bool:
        """Acquire a configuration lock.

        Args:
            project_dir: Absolute path to project directory
            environment: Build environment name
            port: Serial port for the configuration
            lock_type: Type of lock ("exclusive" or "shared_read")
            timeout: Maximum time to wait for the lock in seconds
            description: Human-readable description of the operation

        Returns:
            True if lock was acquired, False otherwise
        """
        response = await self.send_request(
            MessageType.LOCK_ACQUIRE,
            {
                "project_dir": project_dir,
                "environment": environment,
                "port": port,
                "lock_type": lock_type,
                "timeout": timeout,
                "description": description,
            },
            timeout=timeout + 10.0,  # Add buffer for response
        )
        return response.get("success", False)

    async def release(
        self,
        project_dir: str,
        environment: str,
        port: str,
    ) -> bool:
        """Release a configuration lock.

        Args:
            project_dir: Absolute path to project directory
            environment: Build environment name
            port: Serial port for the configuration

        Returns:
            True if lock was released, False otherwise
        """
        response = await self.send_request(
            MessageType.LOCK_RELEASE,
            {
                "project_dir": project_dir,
                "environment": environment,
                "port": port,
            },
        )
        return response.get("success", False)

    async def get_status(
        self,
        project_dir: str,
        environment: str,
        port: str,
    ) -> dict[str, Any]:
        """Get the status of a configuration lock.

        Args:
            project_dir: Absolute path to project directory
            environment: Build environment name
            port: Serial port for the configuration

        Returns:
            Dictionary with lock status information
        """
        return await self.send_request(
            MessageType.LOCK_STATUS,
            {
                "project_dir": project_dir,
                "environment": environment,
                "port": port,
            },
        )
