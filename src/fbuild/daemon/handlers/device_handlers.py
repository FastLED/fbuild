"""
Device operation handlers for the async daemon server.

Handles device list, lease, release, preempt, and status operations.
"""

from __future__ import annotations

import logging
from typing import TYPE_CHECKING, Any

from fbuild.daemon.handlers.base_handler import HandlerContext

if TYPE_CHECKING:
    from fbuild.daemon.async_server import ClientConnection

# Import SubscriptionType and MessageType for broadcast and messaging
from fbuild.daemon.async_server import SubscriptionType


class DeviceListHandler:
    """Handler for device list requests."""

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
        """Handle device list request.

        Args:
            client: The client connection (unused but required for handler signature)
            data: List request data (include_disconnected, refresh)

        Returns:
            Response with device list
        """
        include_disconnected = data.get("include_disconnected", False)
        refresh = data.get("refresh", False)

        # Check that device manager is available
        if self.context.device_manager is None:
            return {
                "success": False,
                "message": "Device manager not available",
                "devices": [],
                "total_devices": 0,
                "connected_devices": 0,
                "total_leases": 0,
            }

        try:
            # Refresh device inventory if requested
            if refresh:
                self.context.device_manager.refresh_devices()

            # Get device status
            all_status = self.context.device_manager.get_all_leases()

            # Filter devices based on include_disconnected
            devices = []
            for _device_id, device_state in all_status.get("devices", {}).items():
                if include_disconnected or device_state.get("is_connected", False):
                    devices.append(device_state)

            return {
                "success": True,
                "message": f"Found {len(devices)} device(s)",
                "devices": devices,
                "total_devices": all_status.get("total_devices", 0),
                "connected_devices": all_status.get("connected_devices", 0),
                "total_leases": all_status.get("total_leases", 0),
            }

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error listing devices: {e}")
            return {
                "success": False,
                "message": f"Device list error: {e}",
                "devices": [],
                "total_devices": 0,
                "connected_devices": 0,
                "total_leases": 0,
            }


class DeviceLeaseHandler:
    """Handler for device lease requests."""

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
        """Handle device lease request.

        Args:
            client: The client connection
            data: Lease request data (device_id, lease_type, description, allows_monitors, timeout)

        Returns:
            Response with lease result
        """
        from fbuild.daemon.device_manager import LeaseType

        device_id = data.get("device_id", "")
        lease_type_str = data.get("lease_type", "exclusive")
        description = data.get("description", "")
        allows_monitors = data.get("allows_monitors", True)
        timeout = data.get("timeout", 300.0)

        if not device_id:
            return {
                "success": False,
                "message": "device_id is required",
            }

        # Check that device manager is available
        if self.context.device_manager is None:
            return {
                "success": False,
                "message": "Device manager not available",
            }

        try:
            lease_type = LeaseType(lease_type_str)
        except ValueError:
            return {
                "success": False,
                "message": f"Invalid lease type: {lease_type_str}. Must be 'exclusive' or 'monitor'",
            }

        try:
            if lease_type == LeaseType.EXCLUSIVE:
                lease = self.context.device_manager.acquire_exclusive(
                    device_id=device_id,
                    client_id=client.client_id,
                    description=description,
                    allows_monitors=allows_monitors,
                    timeout=timeout,
                )
            else:  # MONITOR
                lease = self.context.device_manager.acquire_monitor(
                    device_id=device_id,
                    client_id=client.client_id,
                    description=description,
                )

            if lease:
                logging.info(f"Client {client.client_id} acquired {lease_type.value} lease for device {device_id} (lease_id={lease.lease_id})")

                # Broadcast lease event
                await self.context.broadcast(
                    SubscriptionType.DEVICES,
                    {
                        "event": "lease_acquired",
                        "client_id": client.client_id,
                        "device_id": device_id,
                        "lease_id": lease.lease_id,
                        "lease_type": lease_type.value,
                    },
                    None,  # exclude_client_id
                )

                return {
                    "success": True,
                    "message": f"{lease_type.value} lease acquired",
                    "lease_id": lease.lease_id,
                    "device_id": device_id,
                    "lease_type": lease_type.value,
                    "allows_monitors": lease.allows_monitors,
                }
            else:
                device_status = self.context.device_manager.get_device_status(device_id)
                return {
                    "success": False,
                    "message": "Lease not available",
                    "device_id": device_id,
                    "lease_type": lease_type.value,
                    "is_connected": device_status.get("is_connected", False),
                    "has_exclusive": device_status.get("exclusive_lease") is not None,
                }

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error acquiring device lease for {client.client_id}: {e}")
            return {
                "success": False,
                "message": f"Device lease error: {e}",
            }


class DeviceReleaseHandler:
    """Handler for device lease release requests."""

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
        """Handle device lease release request.

        Args:
            client: The client connection
            data: Release request data (lease_id)

        Returns:
            Response with release result
        """
        lease_id = data.get("lease_id", "")

        if not lease_id:
            return {
                "success": False,
                "message": "lease_id is required",
            }

        # Check that device manager is available
        if self.context.device_manager is None:
            return {
                "success": False,
                "message": "Device manager not available",
            }

        try:
            released = self.context.device_manager.release_lease(lease_id, client.client_id)

            if released:
                logging.info(f"Client {client.client_id} released lease {lease_id}")

                # Broadcast lease release event
                await self.context.broadcast(
                    SubscriptionType.DEVICES,
                    {
                        "event": "lease_released",
                        "client_id": client.client_id,
                        "lease_id": lease_id,
                    },
                    None,  # exclude_client_id
                )

                return {
                    "success": True,
                    "message": "Lease released",
                    "lease_id": lease_id,
                }
            else:
                return {
                    "success": False,
                    "message": "Lease not found or not owned by this client",
                    "lease_id": lease_id,
                }

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error releasing device lease {lease_id} for {client.client_id}: {e}")
            return {
                "success": False,
                "message": f"Device release error: {e}",
            }


class DevicePreemptHandler:
    """Handler for device preemption requests."""

    def __init__(self, context: HandlerContext, get_client_async: Any) -> None:
        """Initialize handler with context.

        Args:
            context: Handler context with dependencies
            get_client_async: Async function to get client by ID
        """
        self.context = context
        self.get_client_async = get_client_async

    async def handle(
        self,
        client: "ClientConnection",
        data: dict[str, Any],
    ) -> dict[str, Any]:
        """Handle device preemption request.

        Forcibly takes the exclusive lease from the current holder.
        The reason is REQUIRED and must not be empty.

        Args:
            client: The client connection
            data: Preempt request data (device_id, reason)

        Returns:
            Response with preemption result
        """
        device_id = data.get("device_id", "")
        reason = data.get("reason", "")

        if not device_id:
            return {
                "success": False,
                "message": "device_id is required",
            }

        if not reason or not reason.strip():
            return {
                "success": False,
                "message": "reason is required and must not be empty",
            }

        # Check that device manager is available
        if self.context.device_manager is None:
            return {
                "success": False,
                "message": "Device manager not available",
            }

        try:
            success, preempted_client_id = self.context.device_manager.preempt_device(
                device_id=device_id,
                requesting_client_id=client.client_id,
                reason=reason,
            )

            if success:
                logging.warning(f"PREEMPTION: {client.client_id} took device {device_id} from {preempted_client_id}. Reason: {reason}")

                # Broadcast preemption event to all subscribers
                await self.context.broadcast(
                    SubscriptionType.DEVICES,
                    {
                        "event": "device_preempted",
                        "device_id": device_id,
                        "preempted_by": client.client_id,
                        "preempted_client_id": preempted_client_id,
                        "reason": reason,
                    },
                    None,  # exclude_client_id
                )

                # Send direct notification to preempted client if they're still connected
                if preempted_client_id:
                    preempted_client = await self.get_client_async(preempted_client_id)
                    if preempted_client:
                        # Import here to avoid circular dependency

                        # We need to send message directly - this is a bit awkward
                        # because we need access to _send_message from the server
                        # For now, we'll just rely on the broadcast above
                        pass

                # Get the new lease for the requester
                device_status = self.context.device_manager.get_device_status(device_id)
                new_lease = device_status.get("exclusive_lease")

                return {
                    "success": True,
                    "message": f"Device preempted from {preempted_client_id}",
                    "device_id": device_id,
                    "preempted_client_id": preempted_client_id,
                    "lease_id": new_lease.get("lease_id") if new_lease else None,
                    "lease_type": "exclusive",
                }
            else:
                return {
                    "success": False,
                    "message": "Preemption failed - device may not have an exclusive holder",
                    "device_id": device_id,
                }

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error preempting device {device_id} for {client.client_id}: {e}")
            return {
                "success": False,
                "message": f"Device preemption error: {e}",
            }


class DeviceStatusHandler:
    """Handler for device status requests."""

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
        """Handle device status request.

        Args:
            client: The client connection (unused but required for handler signature)
            data: Status request data (device_id)

        Returns:
            Response with device status
        """
        device_id = data.get("device_id", "")

        if not device_id:
            return {
                "success": False,
                "message": "device_id is required",
            }

        # Check that device manager is available
        if self.context.device_manager is None:
            return {
                "success": False,
                "message": "Device manager not available",
                "device_id": device_id,
                "exists": False,
            }

        try:
            status = self.context.device_manager.get_device_status(device_id)

            if not status.get("exists", False):
                return {
                    "success": True,
                    "message": "Device not found",
                    "device_id": device_id,
                    "exists": False,
                    "is_connected": False,
                }

            return {
                "success": True,
                "message": "Device status retrieved",
                **status,
            }

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error getting device status for {device_id}: {e}")
            return {
                "success": False,
                "message": f"Device status error: {e}",
                "device_id": device_id,
            }
