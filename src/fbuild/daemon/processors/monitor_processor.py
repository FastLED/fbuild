"""
Monitor Request Processor - Handles serial monitoring operations.

This module implements the MonitorRequestProcessor which executes serial
monitoring operations for Arduino/ESP32 devices. It captures serial output,
performs pattern matching, and handles halt conditions.

Enhanced in Iteration 2 with:
- SharedSerialManager integration for centralized serial port management
- Support for multiple reader clients (broadcast output)
- ConfigurationLockManager for centralized locking
"""

import logging
import sys
import uuid
from pathlib import Path
from typing import TYPE_CHECKING

from fbuild.daemon.messages import DaemonState, OperationType
from fbuild.daemon.port_state_manager import PortState
from fbuild.daemon.request_processor import RequestProcessor

if TYPE_CHECKING:
    from fbuild.daemon.daemon_context import DaemonContext
    from fbuild.daemon.messages import MonitorRequest


class MonitorRequestProcessor(RequestProcessor):
    """Processor for monitor requests.

    This processor handles serial monitoring of Arduino/ESP32 devices. It:
    1. Connects to the specified serial port
    2. Captures and streams output to a file
    3. Performs pattern matching on the output
    4. Handles halt conditions (error/success patterns)
    5. Times out if specified

    The monitor runs until:
    - A halt pattern is matched (halt_on_error or halt_on_success)
    - The timeout is reached
    - The user interrupts it (Ctrl+C)
    - An error occurs

    Example:
        >>> processor = MonitorRequestProcessor()
        >>> success = processor.process_request(monitor_request, daemon_context)
    """

    def get_operation_type(self) -> OperationType:
        """Return MONITOR operation type."""
        return OperationType.MONITOR

    def get_required_locks(self, request: "MonitorRequest", context: "DaemonContext") -> dict[str, str]:
        """Monitor operations require only a port lock.

        Args:
            request: The monitor request
            context: The daemon context

        Returns:
            Dictionary with port lock requirement
        """
        return {"port": request.port} if request.port else {}

    def validate_request(self, request: "MonitorRequest", context: "DaemonContext") -> bool:
        """Validate that the monitor request has a port specified.

        Args:
            request: The monitor request
            context: The daemon context

        Returns:
            True if request is valid (has port), False otherwise
        """
        if not request.port:
            logging.error("Monitor requires port to be specified")
            return False
        return True

    def get_starting_state(self) -> DaemonState:
        """Monitor starts in MONITORING state."""
        return DaemonState.MONITORING

    def get_starting_message(self, request: "MonitorRequest") -> str:
        """Get the starting status message."""
        return f"Monitoring {request.environment} on {request.port}"

    def get_success_message(self, request: "MonitorRequest") -> str:
        """Get the success status message."""
        return "Monitor completed"

    def get_failure_message(self, request: "MonitorRequest") -> str:
        """Get the failure status message."""
        return "Monitor failed"

    def execute_operation(self, request: "MonitorRequest", context: "DaemonContext") -> bool:
        """Execute the serial monitoring operation.

        This is the core monitor logic extracted from the original
        process_monitor_request function. All boilerplate (locks, status
        updates, error handling) is handled by the base RequestProcessor.

        Enhanced in Iteration 2:
        - If SharedSerialManager has the port open, we can attach as a reader
          to get shared access (multiple clients can monitor the same port)
        - Falls back to direct serial access for exclusive monitoring

        Args:
            request: The monitor request containing port, baud_rate, etc.
            context: The daemon context with all subsystems

        Returns:
            True if monitoring completed successfully, False otherwise
        """
        logging.info(f"Starting monitor on {request.port}")

        # Track port state as MONITORING
        port = request.port
        if port:
            context.port_state_manager.acquire_port(
                port=port,
                state=PortState.MONITORING,
                client_pid=request.caller_pid,
                project_dir=request.project_dir,
                environment=request.environment,
                operation_id=request.request_id,
            )

        # Generate a client ID for shared serial manager
        client_id = f"monitor_{request.request_id}_{uuid.uuid4().hex[:8]}"

        # Check if port is already managed by SharedSerialManager
        shared_session = context.shared_serial_manager.get_session_info(port) if port else None

        try:
            if shared_session and shared_session.get("is_open"):
                # Port is already open - attach as a reader for shared access
                assert port is not None  # Guaranteed by shared_session check above
                return self._monitor_shared(request, port, client_id, context)
            else:
                # Port not managed - use direct serial access
                return self._monitor_direct(request, port, context)

        finally:
            # Always clean up
            if port:
                # Detach from shared session if we were attached
                if shared_session and shared_session.get("is_open"):
                    context.shared_serial_manager.detach_reader(port, client_id)
                # Release port state
                context.port_state_manager.release_port(port)

    def _monitor_shared(
        self,
        request: "MonitorRequest",
        port: str,
        client_id: str,
        context: "DaemonContext",
    ) -> bool:
        """Monitor using SharedSerialManager for shared access.

        This allows multiple clients to monitor the same port simultaneously,
        receiving broadcast output from the daemon-managed serial session.

        Args:
            request: The monitor request
            port: Serial port to monitor
            client_id: Client ID for the shared session
            context: The daemon context

        Returns:
            True if monitoring completed successfully
        """
        import time

        logging.info(f"Using shared serial access for {port}")

        # Attach as a reader
        if not context.shared_serial_manager.attach_reader(port, client_id):
            logging.error(f"Failed to attach as reader to {port}")
            return False

        # Create output file path for streaming
        output_file = Path(request.project_dir) / ".fbuild" / "monitor_output.txt"
        output_file.parent.mkdir(parents=True, exist_ok=True)
        output_file.write_text("", encoding="utf-8")

        # Monitor loop - poll the buffer and check for patterns
        start_time = time.time()
        timeout = request.timeout if request.timeout else float("inf")
        last_buffer_size = 0

        try:
            with output_file.open("a", encoding="utf-8") as f:
                while True:
                    # Check timeout
                    elapsed = time.time() - start_time
                    if elapsed >= timeout:
                        logging.info(f"Monitor timeout after {elapsed:.1f}s")
                        return True

                    # Read buffered output
                    lines = context.shared_serial_manager.read_buffer(port, client_id, max_lines=1000)

                    # Process new lines
                    if len(lines) > last_buffer_size:
                        new_lines = lines[last_buffer_size:]
                        for line in new_lines:
                            # Write to output file
                            f.write(line + "\n")
                            f.flush()

                            # Check halt patterns
                            if request.halt_on_error and request.halt_on_error in line:
                                logging.error(f"Halt on error pattern matched: {line}")
                                return False
                            if request.halt_on_success and request.halt_on_success in line:
                                logging.info(f"Halt on success pattern matched: {line}")
                                return True

                        last_buffer_size = len(lines)

                    # Brief sleep to avoid busy-waiting
                    time.sleep(0.1)

        except KeyboardInterrupt:  # noqa: KBI002
            logging.info("Monitor interrupted by user")
            return True

    def _monitor_direct(self, request: "MonitorRequest", port: str | None, context: "DaemonContext") -> bool:
        """Monitor using direct serial access (exclusive).

        This is the original monitoring approach using SerialMonitor.

        Args:
            request: The monitor request
            port: Serial port to monitor
            context: The daemon context (unused but passed for consistency)

        Returns:
            True if monitoring completed successfully
        """
        # Create output file path for streaming
        output_file = Path(request.project_dir) / ".fbuild" / "monitor_output.txt"
        output_file.parent.mkdir(parents=True, exist_ok=True)
        # Clear/truncate output file before starting
        output_file.write_text("", encoding="utf-8")

        # Create summary file path
        summary_file = Path(request.project_dir) / ".fbuild" / "monitor_summary.json"
        # Clear old summary file
        if summary_file.exists():
            summary_file.unlink()

        try:
            # Get fresh monitor class after module reload
            # Using direct import would use cached version
            monitor_class = getattr(sys.modules["fbuild.deploy.monitor"], "SerialMonitor")
        except (KeyError, AttributeError) as e:
            logging.error(f"Failed to get SerialMonitor class: {e}")
            return False

        # Create monitor and execute
        monitor = monitor_class(verbose=False)
        exit_code = monitor.monitor(
            project_dir=Path(request.project_dir),
            env_name=request.environment,
            port=request.port,
            baud=request.baud_rate if request.baud_rate else 115200,
            timeout=int(request.timeout) if request.timeout is not None else None,
            halt_on_error=request.halt_on_error,
            halt_on_success=request.halt_on_success,
            expect=request.expect,
            output_file=output_file,
            summary_file=summary_file,
        )

        if exit_code == 0:
            logging.info("Monitor completed successfully")
            return True
        else:
            logging.error(f"Monitor failed with exit code {exit_code}")
            return False
