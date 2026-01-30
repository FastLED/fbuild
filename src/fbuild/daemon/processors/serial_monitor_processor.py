"""Serial Monitor API Processor - Handles fbuild.api.SerialMonitor requests.

This module processes requests from the SerialMonitor Python API, enabling
external scripts to monitor serial output through the daemon without holding
OS-level port locks.

The processor handles:
- Attach: Attach as reader to daemon serial session
- Detach: Detach from daemon serial session
- Poll: Get new output lines since last poll (incremental)
- Write: Write data to serial port (uses existing SerialWriteRequest)

Key Features:
- Incremental polling (clients track last_index to avoid re-reading)
- Automatic port opening if needed (open_if_needed flag)
- Preemption detection (deploy can notify API monitors)
- Thread-safe access through SharedSerialManager

Example IPC Flow:
    SerialMonitor.attach()
        → writes serial_monitor_attach_request.json
        → daemon polls file, calls handle_attach()
        → daemon writes serial_monitor_response.json
        ← SerialMonitor reads response

    SerialMonitor.read_lines()
        → writes serial_monitor_poll_request.json (every 100ms)
        → daemon polls file, calls handle_poll()
        → daemon writes serial_monitor_response.json
        ← SerialMonitor reads response, yields lines
"""

import logging
from typing import TYPE_CHECKING

from fbuild.daemon.messages import (
    SerialMonitorAttachRequest,
    SerialMonitorDetachRequest,
    SerialMonitorPollRequest,
    SerialMonitorResponse,
    SerialWriteRequest,
)

if TYPE_CHECKING:
    from fbuild.daemon.daemon_context import DaemonContext


class SerialMonitorAPIProcessor:
    """Processor for Serial Monitor API requests.

    Handles API-based serial monitor operations that route through the daemon
    to eliminate OS-level port locks. Works alongside SerialSessionProcessor
    (which handles CLI monitor operations).
    """

    def handle_attach(
        self,
        request: SerialMonitorAttachRequest,
        context: "DaemonContext",
    ) -> SerialMonitorResponse:
        """Handle API attach request.

        Opens port if needed and attaches client as reader.

        Args:
            request: Attach request with port, baud_rate, client_id
            context: Daemon context with shared serial manager

        Returns:
            SerialMonitorResponse with success/failure status
        """
        manager = context.shared_serial_manager

        logging.info(f"[SerialMonitor] Attach request: client={request.client_id}, port={request.port}, baud={request.baud_rate}")

        try:
            # Check if port is already open
            session_info = manager.get_session_info(request.port)

            if session_info and session_info.get("is_open", False):
                # Port already open, just attach as reader
                success = manager.attach_reader(request.port, request.client_id)
                if success:
                    # Track resource attachment for cleanup
                    resource_key = f"serial_monitor:{request.port}"
                    context.client_manager.attach_resource(request.client_id, resource_key)

                    logging.info(f"[SerialMonitor] Client {request.client_id} attached to existing session on {request.port}")
                    return SerialMonitorResponse(
                        request_id=request.request_id,
                        success=True,
                        message=f"Attached to existing session on {request.port}",
                    )
                else:
                    logging.error(f"[SerialMonitor] Failed to attach client {request.client_id} to {request.port}")
                    return SerialMonitorResponse(
                        request_id=request.request_id,
                        success=False,
                        message="Failed to attach to session",
                    )

            elif request.open_if_needed:
                # Port not open, open it and attach
                success = manager.open_port(
                    port=request.port,
                    baud_rate=request.baud_rate,
                    client_id=request.client_id,
                )

                if not success:
                    logging.error(f"[SerialMonitor] Failed to open port {request.port}")
                    return SerialMonitorResponse(
                        request_id=request.request_id,
                        success=False,
                        message=f"Failed to open port {request.port}",
                    )

                # Port opened, now attach as reader
                success = manager.attach_reader(request.port, request.client_id)
                if success:
                    # Track resource attachment
                    resource_key = f"serial_monitor:{request.port}"
                    context.client_manager.attach_resource(request.client_id, resource_key)

                    logging.info(f"[SerialMonitor] Opened {request.port} and attached client {request.client_id}")
                    return SerialMonitorResponse(
                        request_id=request.request_id,
                        success=True,
                        message=f"Opened port {request.port} and attached",
                    )
                else:
                    # Failed to attach after opening (shouldn't happen)
                    logging.error(f"[SerialMonitor] Opened {request.port} but failed to attach client {request.client_id}")
                    return SerialMonitorResponse(
                        request_id=request.request_id,
                        success=False,
                        message="Failed to attach after opening port",
                    )

            else:
                # Port not open and open_if_needed=False
                logging.warning(f"[SerialMonitor] Port {request.port} not open and open_if_needed=False")
                return SerialMonitorResponse(
                    request_id=request.request_id,
                    success=False,
                    message=f"Port {request.port} not open (open_if_needed=False)",
                )

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"[SerialMonitor] Error in attach: {e}")
            return SerialMonitorResponse(
                request_id=request.request_id,
                success=False,
                message=f"Error: {e}",
            )

    def handle_detach(
        self,
        request: SerialMonitorDetachRequest,
        context: "DaemonContext",
    ) -> SerialMonitorResponse:
        """Handle API detach request.

        Detaches client from serial session. Does NOT close the port
        (other clients may still be attached).

        Args:
            request: Detach request with port, client_id
            context: Daemon context with shared serial manager

        Returns:
            SerialMonitorResponse with success/failure status
        """
        manager = context.shared_serial_manager

        logging.info(f"[SerialMonitor] Detach request: client={request.client_id}, port={request.port}")

        try:
            # Detach reader
            success = manager.detach_reader(request.port, request.client_id)

            if success:
                # Remove resource tracking
                resource_key = f"serial_monitor:{request.port}"
                context.client_manager.detach_resource(request.client_id, resource_key)

                logging.info(f"[SerialMonitor] Client {request.client_id} detached from {request.port}")
                return SerialMonitorResponse(
                    request_id=request.request_id,
                    success=True,
                    message=f"Detached from {request.port}",
                )
            else:
                logging.warning(f"[SerialMonitor] Client {request.client_id} not attached to {request.port}")
                return SerialMonitorResponse(
                    request_id=request.request_id,
                    success=False,
                    message="Not attached to session",
                )

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"[SerialMonitor] Error in detach: {e}")
            return SerialMonitorResponse(
                request_id=request.request_id,
                success=False,
                message=f"Error: {e}",
            )

    def handle_poll(
        self,
        request: SerialMonitorPollRequest,
        context: "DaemonContext",
    ) -> SerialMonitorResponse:
        """Handle API poll request.

        Returns new lines from buffer since last_index.

        Args:
            request: Poll request with port, client_id, last_index, max_lines
            context: Daemon context with shared serial manager

        Returns:
            SerialMonitorResponse with lines and current_index
        """
        manager = context.shared_serial_manager

        # Use debug level for poll to avoid log spam
        logging.debug(f"[SerialMonitor] Poll request: client={request.client_id}, port={request.port}, last_index={request.last_index}")

        try:
            # Verify client is attached as reader
            session_info = manager.get_session_info(request.port)
            if not session_info:
                return SerialMonitorResponse(
                    request_id=request.request_id,
                    success=False,
                    message=f"Port {request.port} not open",
                )

            reader_ids = session_info.get("reader_client_ids", [])
            if request.client_id not in reader_ids:
                return SerialMonitorResponse(
                    request_id=request.request_id,
                    success=False,
                    message="Not attached as reader",
                )

            # Get buffer
            all_lines = manager.read_buffer(request.port, request.client_id, max_lines=10000)

            # Calculate new lines since last_index
            # Note: Buffer is circular, so we need to slice carefully
            buffer_size = len(all_lines)
            current_index = buffer_size  # New index for next poll

            if request.last_index >= buffer_size:
                # Client is caught up, no new lines
                new_lines = []
            elif request.last_index < 0:
                # Invalid index, return all lines
                new_lines = all_lines[: request.max_lines]
            else:
                # Return lines from last_index to end
                new_lines = all_lines[request.last_index : request.last_index + request.max_lines]

            if new_lines:
                logging.debug(f"[SerialMonitor] Returning {len(new_lines)} new lines (last_index={request.last_index}, current={current_index})")

            return SerialMonitorResponse(
                request_id=request.request_id,
                success=True,
                message="Poll successful",
                lines=new_lines,
                current_index=current_index,
            )

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"[SerialMonitor] Error in poll: {e}")
            return SerialMonitorResponse(
                request_id=request.request_id,
                success=False,
                message=f"Error: {e}",
            )

    def handle_write(
        self,
        request: SerialWriteRequest,
        context: "DaemonContext",
    ) -> SerialMonitorResponse:
        """Handle API write request.

        Writes data to serial port. Automatically acquires/releases writer lock.

        Args:
            request: Write request with port, client_id, data (base64)
            context: Daemon context with shared serial manager

        Returns:
            SerialMonitorResponse with bytes_written count
        """
        manager = context.shared_serial_manager

        logging.info(f"[SerialMonitor] Write: client={request.client_id}, port={request.port}, acquire_writer={request.acquire_writer}")

        try:
            # Decode data from base64
            import base64

            data = base64.b64decode(request.data)
            logging.debug(f"[SerialMonitor] Write: decoded {len(data)} bytes from base64")

            # Note: SerialWriteRequest doesn't have request_id yet
            # For now, use empty string for compatibility
            req_id = getattr(request, "request_id", "")

            # Acquire writer if requested
            if request.acquire_writer:
                logging.info(f"[SerialMonitor] Write: acquiring writer lock for {request.port}")
                success = manager.acquire_writer(request.port, request.client_id, timeout=10.0)
                if not success:
                    return SerialMonitorResponse(
                        request_id=req_id,
                        success=False,
                        message="Failed to acquire writer lock",
                    )
                logging.info(f"[SerialMonitor] Write: acquired writer lock for {request.port}")

            try:
                # Write data
                logging.info(f"[SerialMonitor] Write: calling manager.write() for {request.port}")
                bytes_written = manager.write(request.port, request.client_id, data)
                logging.info(f"[SerialMonitor] Write: manager.write() returned {bytes_written}")

                if bytes_written < 0:
                    return SerialMonitorResponse(
                        request_id=req_id,
                        success=False,
                        message="Write failed",
                    )

                logging.info(f"[SerialMonitor] Wrote {bytes_written} bytes to {request.port}")
                return SerialMonitorResponse(
                    request_id=req_id,
                    success=True,
                    message=f"Wrote {bytes_written} bytes",
                    bytes_written=bytes_written,
                )

            finally:
                # Release writer if we acquired it
                if request.acquire_writer:
                    manager.release_writer(request.port, request.client_id)

        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"[SerialMonitor] Error in write: {e}")
            req_id = getattr(request, "request_id", "") if "request" in locals() else ""
            return SerialMonitorResponse(
                request_id=req_id,
                success=False,
                message=f"Error: {e}",
            )
