"""
Monitor Request Processor - Handles serial monitoring operations.

This module implements the MonitorRequestProcessor which executes serial
monitoring operations for Arduino/ESP32 devices. It captures serial output,
performs pattern matching, and handles halt conditions.
"""

import logging
import sys
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

        try:
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

        finally:
            # Release port state when monitoring completes
            if port:
                context.port_state_manager.release_port(port)
