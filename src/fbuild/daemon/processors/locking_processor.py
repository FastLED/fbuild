"""
Locking Request Processor - Handles lock management, firmware queries, and serial session requests.

This module provides processors for the new centralized locking mechanism messages:
- Lock management (acquire, release, status)
- Firmware ledger queries (check if firmware is current)
- Serial session management (attach, detach, write, read buffer)
- Client connection management (connect, heartbeat, disconnect)
"""

import base64
import logging
import time
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

from fbuild.daemon.messages import (
    ClientConnectRequest,
    ClientDisconnectRequest,
    ClientHeartbeatRequest,
    ClientResponse,
    FirmwareQueryRequest,
    FirmwareQueryResponse,
    FirmwareRecordRequest,
    LockAcquireRequest,
    LockReleaseRequest,
    LockResponse,
    LockStatusRequest,
    LockType,
    SerialAttachRequest,
    SerialBufferRequest,
    SerialDetachRequest,
    SerialSessionResponse,
    SerialWriteRequest,
)

if TYPE_CHECKING:
    from fbuild.daemon.daemon_context import DaemonContext


@dataclass
class GenericResponse:
    """Generic response for operations without a specific response type."""

    success: bool
    message: str
    data: dict[str, Any] = field(default_factory=dict)
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return {
            "success": self.success,
            "message": self.message,
            "data": self.data,
            "timestamp": self.timestamp,
        }


class LockingRequestProcessor:
    """Processor for lock management requests.

    Handles lock acquire, release, and status requests using the
    ConfigurationLockManager from the daemon context.
    """

    def handle_lock_acquire(
        self,
        request: LockAcquireRequest,
        context: "DaemonContext",
    ) -> LockResponse:
        """Handle a lock acquisition request.

        Args:
            request: Lock acquisition request
            context: Daemon context with configuration lock manager

        Returns:
            LockResponse indicating success or failure
        """
        config_key = (request.project_dir, request.environment, request.port)
        manager = context.configuration_lock_manager

        logging.info(f"Lock acquire request: client={request.client_id}, config={config_key}, type={request.lock_type.value}")

        try:
            if request.lock_type == LockType.EXCLUSIVE:
                success = manager.acquire_exclusive(
                    config_key=config_key,
                    client_id=request.client_id,
                    description=request.description,
                    timeout=request.timeout,
                )
            else:  # SHARED_READ
                success = manager.acquire_shared_read(
                    config_key=config_key,
                    client_id=request.client_id,
                    description=request.description,
                )

            if success:
                # Track resource attachment for cleanup
                resource_key = f"lock:{request.project_dir}|{request.environment}|{request.port}"
                context.client_manager.attach_resource(request.client_id, resource_key)

                lock_status = manager.get_lock_status(config_key)
                return LockResponse(
                    success=True,
                    message=f"Lock acquired ({request.lock_type.value})",
                    lock_state=lock_status.get("state", "unknown"),
                    holder_count=lock_status.get("holder_count", 0),
                    waiting_count=lock_status.get("waiting_count", 0),
                )
            else:
                lock_status = manager.get_lock_status(config_key)
                return LockResponse(
                    success=False,
                    message="Lock not available",
                    lock_state=lock_status.get("state", "unknown"),
                    holder_count=lock_status.get("holder_count", 0),
                    waiting_count=lock_status.get("waiting_count", 0),
                )

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error acquiring lock: {e}")
            return LockResponse(
                success=False,
                message=f"Error acquiring lock: {e}",
            )

    def handle_lock_release(
        self,
        request: LockReleaseRequest,
        context: "DaemonContext",
    ) -> LockResponse:
        """Handle a lock release request.

        Args:
            request: Lock release request
            context: Daemon context with configuration lock manager

        Returns:
            LockResponse indicating success or failure
        """
        config_key = (request.project_dir, request.environment, request.port)
        manager = context.configuration_lock_manager

        logging.info(f"Lock release request: client={request.client_id}, config={config_key}")

        try:
            success = manager.release(config_key, request.client_id)

            if success:
                # Remove resource tracking
                resource_key = f"lock:{request.project_dir}|{request.environment}|{request.port}"
                context.client_manager.detach_resource(request.client_id, resource_key)

            lock_status = manager.get_lock_status(config_key)

            return LockResponse(
                success=success,
                message="Lock released" if success else "No lock held by client",
                lock_state=lock_status.get("state", "unlocked"),
                holder_count=lock_status.get("holder_count", 0),
                waiting_count=lock_status.get("waiting_count", 0),
            )

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error releasing lock: {e}")
            return LockResponse(
                success=False,
                message=f"Error releasing lock: {e}",
            )

    def handle_lock_status(
        self,
        request: LockStatusRequest,
        context: "DaemonContext",
    ) -> LockResponse:
        """Handle a lock status query request.

        Args:
            request: Lock status request
            context: Daemon context with configuration lock manager

        Returns:
            LockResponse with current lock state
        """
        config_key = (request.project_dir, request.environment, request.port)
        manager = context.configuration_lock_manager

        logging.debug(f"Lock status request: config={config_key}")

        try:
            lock_status = manager.get_lock_status(config_key)

            return LockResponse(
                success=True,
                message="Lock status retrieved",
                lock_state=lock_status.get("state", "unlocked"),
                holder_count=lock_status.get("holder_count", 0),
                waiting_count=lock_status.get("waiting_count", 0),
            )

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error getting lock status: {e}")
            return LockResponse(
                success=False,
                message=f"Error getting lock status: {e}",
            )


class FirmwareRequestProcessor:
    """Processor for firmware ledger requests.

    Handles firmware query and record requests using the FirmwareLedger
    from the daemon context.
    """

    def handle_firmware_query(
        self,
        request: FirmwareQueryRequest,
        context: "DaemonContext",
    ) -> FirmwareQueryResponse:
        """Handle a firmware query request.

        Checks if the current firmware on a device matches the expected
        source and build flags hashes.

        Args:
            request: Firmware query request
            context: Daemon context with firmware ledger

        Returns:
            FirmwareQueryResponse with firmware status
        """
        ledger = context.firmware_ledger

        logging.debug(f"Firmware query request: port={request.port}, source_hash={request.source_hash[:16]}...")

        try:
            # Get deployment info if available
            deployment = ledger.get_deployment(request.port)

            if deployment:
                needs_redeploy = ledger.needs_redeploy(
                    port=request.port,
                    source_hash=request.source_hash,
                    build_flags_hash=request.build_flags_hash,
                )

                # is_current means source and build flags match (no redeploy needed)
                is_current = not needs_redeploy

                return FirmwareQueryResponse(
                    is_current=is_current,
                    needs_redeploy=needs_redeploy,
                    firmware_hash=deployment.firmware_hash,
                    project_dir=deployment.project_dir,
                    environment=deployment.environment,
                    upload_timestamp=deployment.upload_timestamp,
                    message="Firmware info retrieved" if is_current else "Redeploy needed",
                )
            else:
                return FirmwareQueryResponse(
                    is_current=False,
                    needs_redeploy=True,
                    message="No firmware recorded for this port",
                )

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error querying firmware: {e}")
            return FirmwareQueryResponse(
                is_current=False,
                needs_redeploy=True,
                message=f"Error querying firmware: {e}",
            )

    def handle_firmware_record(
        self,
        request: FirmwareRecordRequest,
        context: "DaemonContext",
    ) -> GenericResponse:
        """Handle a firmware record request.

        Records a successful deployment in the firmware ledger.

        Args:
            request: Firmware record request
            context: Daemon context with firmware ledger

        Returns:
            GenericResponse indicating success or failure
        """
        ledger = context.firmware_ledger

        logging.info(f"Firmware record request: port={request.port}, project={request.project_dir}, env={request.environment}")

        try:
            ledger.record_deployment(
                port=request.port,
                firmware_hash=request.firmware_hash,
                source_hash=request.source_hash,
                project_dir=request.project_dir,
                environment=request.environment,
                build_flags_hash=request.build_flags_hash,
            )

            return GenericResponse(
                success=True,
                message="Firmware deployment recorded",
                data={
                    "port": request.port,
                    "firmware_hash": request.firmware_hash,
                    "source_hash": request.source_hash,
                },
            )

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error recording firmware: {e}")
            return GenericResponse(
                success=False,
                message=f"Error recording firmware: {e}",
            )


class SerialSessionProcessor:
    """Processor for serial session management requests.

    Handles serial attach, detach, write, and buffer read requests
    using the SharedSerialManager from the daemon context.
    """

    def handle_serial_attach(
        self,
        request: SerialAttachRequest,
        context: "DaemonContext",
    ) -> SerialSessionResponse:
        """Handle a serial attach request.

        Attaches a client to a serial session as a reader, or opens
        a new serial port if not already open.

        Args:
            request: Serial attach request
            context: Daemon context with shared serial manager

        Returns:
            SerialSessionResponse with session status
        """
        manager = context.shared_serial_manager

        logging.info(f"Serial attach request: client={request.client_id}, port={request.port}, as_reader={request.as_reader}")

        try:
            # Check if port is already open
            session_info = manager.get_session_info(request.port) or {}
            if session_info and session_info.get("is_open", False):
                if request.as_reader:
                    # Attach as reader to existing session
                    success = manager.attach_reader(request.port, request.client_id)
                    if success:
                        # Track resource attachment
                        resource_key = f"serial:{request.port}"
                        context.client_manager.attach_resource(request.client_id, resource_key)

                    session_info_new = manager.get_session_info(request.port) or {}
                    return SerialSessionResponse(
                        success=success,
                        message="Attached as reader" if success else "Failed to attach",
                        is_open=True,
                        reader_count=len(session_info_new.get("reader_client_ids", [])),
                        has_writer=session_info_new.get("writer_client_id") is not None,
                        buffer_size=session_info_new.get("buffer_size", 0),
                    )
                else:
                    # Port already open, can't open again
                    return SerialSessionResponse(
                        success=False,
                        message="Port already open by another client",
                        is_open=True,
                        reader_count=len(session_info.get("reader_client_ids", [])),
                        has_writer=session_info.get("writer_client_id") is not None,
                    )
            else:
                # Open new port
                success = manager.open_port(
                    port=request.port,
                    baud_rate=request.baud_rate,
                    client_id=request.client_id,
                )

                if success:
                    # Track resource attachment
                    resource_key = f"serial:{request.port}"
                    context.client_manager.attach_resource(request.client_id, resource_key)

                    new_session_info = manager.get_session_info(request.port) or {}
                    return SerialSessionResponse(
                        success=True,
                        message="Port opened and attached",
                        is_open=True,
                        reader_count=len(new_session_info.get("reader_client_ids", [])),
                        has_writer=new_session_info.get("writer_client_id") is not None,
                        buffer_size=0,
                    )
                else:
                    return SerialSessionResponse(
                        success=False,
                        message="Failed to open port",
                        is_open=False,
                    )

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error in serial attach: {e}")
            return SerialSessionResponse(
                success=False,
                message=f"Error: {e}",
                is_open=False,
            )

    def handle_serial_detach(
        self,
        request: SerialDetachRequest,
        context: "DaemonContext",
    ) -> SerialSessionResponse:
        """Handle a serial detach request.

        Detaches a client from a serial session. Optionally closes the port
        if this is the last reader.

        Args:
            request: Serial detach request
            context: Daemon context with shared serial manager

        Returns:
            SerialSessionResponse with session status
        """
        manager = context.shared_serial_manager

        logging.info(f"Serial detach request: client={request.client_id}, port={request.port}, close_port={request.close_port}")

        try:
            # Detach reader
            success = manager.detach_reader(request.port, request.client_id)

            if success:
                # Remove resource tracking
                resource_key = f"serial:{request.port}"
                context.client_manager.detach_resource(request.client_id, resource_key)

            # Check if we should close the port
            session_info = manager.get_session_info(request.port) or {}
            port_is_open = session_info.get("is_open", False)

            if request.close_port and port_is_open:
                if len(session_info.get("reader_client_ids", [])) == 0:
                    manager.close_port(request.port, request.client_id)
                    return SerialSessionResponse(
                        success=True,
                        message="Detached and port closed",
                        is_open=False,
                        reader_count=0,
                    )

            if port_is_open:
                session_info = manager.get_session_info(request.port) or {}
                return SerialSessionResponse(
                    success=success,
                    message="Detached from session" if success else "Not attached",
                    is_open=True,
                    reader_count=len(session_info.get("reader_client_ids", [])),
                    has_writer=session_info.get("writer_client_id") is not None,
                    buffer_size=session_info.get("buffer_size", 0),
                )
            else:
                return SerialSessionResponse(
                    success=success,
                    message="Detached (port not open)",
                    is_open=False,
                )

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error in serial detach: {e}")
            return SerialSessionResponse(
                success=False,
                message=f"Error: {e}",
            )

    def handle_serial_write(
        self,
        request: SerialWriteRequest,
        context: "DaemonContext",
    ) -> SerialSessionResponse:
        """Handle a serial write request.

        Writes data to a serial port. The client must have writer access
        or request to acquire it.

        Args:
            request: Serial write request (data is base64 encoded)
            context: Daemon context with shared serial manager

        Returns:
            SerialSessionResponse with write status
        """
        manager = context.shared_serial_manager

        logging.debug(f"Serial write request: client={request.client_id}, port={request.port}")

        try:
            # Decode base64 data
            data_bytes = base64.b64decode(request.data)

            # Check if client has writer access
            session_info = manager.get_session_info(request.port) or {}
            current_writer = session_info.get("writer_client_id") if session_info else None

            if current_writer != request.client_id:
                if request.acquire_writer:
                    # Try to acquire writer access
                    acquired = manager.acquire_writer(request.port, request.client_id)
                    if not acquired:
                        return SerialSessionResponse(
                            success=False,
                            message="Could not acquire writer access",
                            is_open=session_info.get("is_open", False),
                            has_writer=session_info.get("writer_client_id") is not None,
                        )
                else:
                    return SerialSessionResponse(
                        success=False,
                        message="No writer access and acquire_writer is False",
                        is_open=session_info.get("is_open", False),
                        has_writer=True,
                    )

            # Write data
            bytes_written = manager.write(request.port, request.client_id, data_bytes)

            return SerialSessionResponse(
                success=bytes_written > 0,
                message=f"Wrote {bytes_written} bytes",
                is_open=True,
                bytes_written=bytes_written,
            )

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error in serial write: {e}")
            return SerialSessionResponse(
                success=False,
                message=f"Error: {e}",
            )

    def handle_serial_buffer(
        self,
        request: SerialBufferRequest,
        context: "DaemonContext",
    ) -> SerialSessionResponse:
        """Handle a serial buffer read request.

        Reads buffered output from a serial session.

        Args:
            request: Serial buffer request
            context: Daemon context with shared serial manager

        Returns:
            SerialSessionResponse with buffered lines
        """
        manager = context.shared_serial_manager

        logging.debug(f"Serial buffer request: client={request.client_id}, port={request.port}, max_lines={request.max_lines}")

        try:
            session_info = manager.get_session_info(request.port) or {}
            if not session_info.get("is_open", False):
                return SerialSessionResponse(
                    success=False,
                    message="Port not open",
                    is_open=False,
                )

            # Read buffer
            lines = manager.read_buffer(
                port=request.port,
                client_id=request.client_id,
                max_lines=request.max_lines,
            )

            session_info = manager.get_session_info(request.port) or {}

            return SerialSessionResponse(
                success=True,
                message=f"Read {len(lines)} lines",
                is_open=True,
                reader_count=len(session_info.get("reader_client_ids", [])),
                has_writer=session_info.get("writer_client_id") is not None,
                buffer_size=session_info.get("buffer_size", 0),
                lines=lines,
            )

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error in serial buffer read: {e}")
            return SerialSessionResponse(
                success=False,
                message=f"Error: {e}",
            )


class ClientConnectionProcessor:
    """Processor for client connection management requests.

    Handles client connect, heartbeat, and disconnect requests using
    the ClientConnectionManager from the daemon context.
    """

    def handle_client_connect(
        self,
        request: ClientConnectRequest,
        context: "DaemonContext",
    ) -> ClientResponse:
        """Handle a client connection request.

        Registers a new client with the daemon.

        Args:
            request: Client connect request
            context: Daemon context with client manager

        Returns:
            ClientResponse with registration status
        """
        manager = context.client_manager

        logging.info(f"Client connect request: client_id={request.client_id}, pid={request.pid}")

        try:
            # Build metadata from request
            metadata = {
                "hostname": request.hostname,
                "version": request.version,
                "connect_timestamp": request.timestamp,
            }

            # Register client
            client_info = manager.register_client(
                client_id=request.client_id,
                pid=request.pid,
                metadata=metadata,
            )

            return ClientResponse(
                success=True,
                message="Client registered",
                client_id=client_info.client_id,
                is_registered=True,
                total_clients=manager.get_client_count(),
            )

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error registering client: {e}")
            return ClientResponse(
                success=False,
                message=f"Error: {e}",
                client_id=request.client_id,
                is_registered=False,
            )

    def handle_client_heartbeat(
        self,
        request: ClientHeartbeatRequest,
        context: "DaemonContext",
    ) -> ClientResponse:
        """Handle a client heartbeat request.

        Updates the last heartbeat time for a client.

        Args:
            request: Client heartbeat request
            context: Daemon context with client manager

        Returns:
            ClientResponse with heartbeat status
        """
        manager = context.client_manager

        logging.debug(f"Client heartbeat: client_id={request.client_id}")

        try:
            success = manager.heartbeat(request.client_id)

            return ClientResponse(
                success=success,
                message="Heartbeat recorded" if success else "Unknown client",
                client_id=request.client_id,
                is_registered=success,
                total_clients=manager.get_client_count(),
            )

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error processing heartbeat: {e}")
            return ClientResponse(
                success=False,
                message=f"Error: {e}",
                client_id=request.client_id,
            )

    def handle_client_disconnect(
        self,
        request: ClientDisconnectRequest,
        context: "DaemonContext",
    ) -> ClientResponse:
        """Handle a client disconnect request.

        Unregisters a client and releases all its resources.

        Args:
            request: Client disconnect request
            context: Daemon context with client manager

        Returns:
            ClientResponse with disconnect status
        """
        manager = context.client_manager

        logging.info(f"Client disconnect request: client_id={request.client_id}, reason={request.reason}")

        try:
            success = manager.unregister_client(request.client_id)

            return ClientResponse(
                success=success,
                message="Client disconnected" if success else "Client not found",
                client_id=request.client_id,
                is_registered=False,
                total_clients=manager.get_client_count(),
            )

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error disconnecting client: {e}")
            return ClientResponse(
                success=False,
                message=f"Error: {e}",
                client_id=request.client_id,
            )


# Create singleton instances for easy access
locking_processor = LockingRequestProcessor()
firmware_processor = FirmwareRequestProcessor()
serial_processor = SerialSessionProcessor()
client_processor = ClientConnectionProcessor()
