"""SerialMonitor API - Daemon-routed serial I/O for external scripts.

This module provides the SerialMonitor context manager which routes all serial
I/O through the fbuild daemon. This eliminates Windows driver-level port locks
that cause PermissionError conflicts between validation scripts and deploy operations.

Key Features:
- Multiple clients can monitor the same port concurrently (shared read access)
- Deploy operations can preempt monitors gracefully (auto-reconnect)
- No OS-level port locks held by client processes
- Simple context manager interface

Example Usage:
    >>> from fbuild.api import SerialMonitor
    >>>
    >>> # Basic monitoring
    >>> with SerialMonitor(port='COM13', baud_rate=115200) as mon:
    ...     for line in mon.read_lines(timeout=30.0):
    ...         print(line)
    ...         if 'ERROR' in line:
    ...             raise RuntimeError('Device error detected')
    >>>
    >>> # With JSON-RPC requests
    >>> def on_line(line: str):
    ...     print(f"[{time.time()}] {line}")
    >>>
    >>> with SerialMonitor(port='COM13', hooks=[on_line]) as mon:
    ...     # Send JSON-RPC request
    ...     mon.write_json_rpc({'method': 'configure', 'params': {'i2s': True}})
    ...     # Wait for expected response
    ...     mon.run_until(lambda: 'CONFIGURED' in mon.last_line, timeout=10.0)

Architecture:
    validate.py (client) → SerialMonitor API → Daemon (SharedSerialManager) → COM13

IPC Mechanism:
    File-based JSON request/response (consistent with build/deploy requests)
    - 100ms polling interval (acceptable for validation use case)
    - Request files: serial_monitor_attach/detach/poll_request.json
    - Response file: serial_monitor_response.json
"""

import json
import logging
import time
import uuid
from collections.abc import Callable, Iterator
from pathlib import Path
from typing import Any

from fbuild.daemon.client.lifecycle import ensure_daemon_running
from fbuild.daemon.messages import (
    SerialMonitorAttachRequest,
    SerialMonitorDetachRequest,
    SerialMonitorPollRequest,
    SerialMonitorResponse,
    SerialWriteRequest,
)
from fbuild.daemon.paths import DAEMON_DIR

# Hook type: callback function that receives each line
MonitorHook = Callable[[str], None]

# Polling interval for reading new lines (100ms)
POLL_INTERVAL = 0.1

# Default timeout for attach/detach operations (60 seconds)
# Increased to 60s to allow SharedSerialManager retry logic to complete
# Windows USB-CDC re-enumeration can take 10-20+ seconds, and retries add overhead
OPERATION_TIMEOUT = 60.0


class MonitorPreemptedException(Exception):
    """Raised when monitor is preempted by deploy operation.

    Only raised if auto_reconnect=False. When auto_reconnect=True, the
    monitor automatically waits for deploy to complete and reconnects.
    """

    def __init__(self, port: str, preempted_by: str):
        """Initialize exception.

        Args:
            port: Port that was preempted
            preempted_by: Client ID that preempted the monitor
        """
        self.port = port
        self.preempted_by = preempted_by
        super().__init__(f"Monitor on {port} preempted by {preempted_by}")


class MonitorHookError(Exception):
    """Raised when a hook callback fails with an exception."""

    def __init__(self, hook: MonitorHook, original_error: Exception):
        """Initialize exception.

        Args:
            hook: The hook that failed
            original_error: The exception raised by the hook
        """
        self.hook = hook
        self.original_error = original_error
        super().__init__(f"Hook {hook.__name__} failed: {original_error}")


class SerialMonitor:
    """Context manager for daemon-routed serial monitoring.

    This class provides a high-level API for monitoring serial output through
    the fbuild daemon. It handles attachment, polling, preemption, and cleanup
    automatically.

    The monitor uses file-based IPC to communicate with the daemon via the
    SharedSerialManager. This eliminates OS-level port locks and enables
    concurrent monitoring by multiple clients.

    Example:
        >>> with SerialMonitor(port='COM13', baud_rate=115200) as mon:
        ...     for line in mon.read_lines(timeout=30.0):
        ...         if 'READY' in line:
        ...             break

    Attributes:
        port: Serial port to monitor
        baud_rate: Baud rate for serial connection
        hooks: List of callback functions invoked for each line
        auto_reconnect: Whether to automatically reconnect after deploy preemption
        verbose: Whether to log verbose debug information
        client_id: Unique identifier for this monitor instance
        last_line: Most recent line received (useful for hooks)
    """

    def __init__(
        self,
        port: str,
        baud_rate: int = 115200,
        hooks: list[MonitorHook] | None = None,
        auto_reconnect: bool = True,
        verbose: bool = False,
    ):
        """Initialize SerialMonitor.

        Args:
            port: Serial port to monitor (e.g., "COM13", "/dev/ttyUSB0")
            baud_rate: Baud rate for serial connection (default: 115200)
            hooks: Optional list of callback functions to invoke for each line.
                   Each hook receives the line as a string argument.
                   Hooks are called in order and can raise exceptions to abort monitoring.
            auto_reconnect: Whether to automatically reconnect after deploy preemption.
                            If True, monitoring pauses during deploy and resumes after.
                            If False, raises MonitorPreemptedException on preemption.
            verbose: Whether to log verbose debug information to console
        """
        self.port = port
        self.baud_rate = baud_rate
        self.hooks = hooks or []
        self.auto_reconnect = auto_reconnect
        self.verbose = verbose

        # Generate unique client ID
        self.client_id = f"serial_monitor_{uuid.uuid4().hex[:8]}"

        # Tracking state
        self._attached = False
        self._last_index = 0  # For incremental polling
        self.last_line = ""  # Most recent line (for hooks/conditions)

        # File paths for IPC
        self._attach_request_file = DAEMON_DIR / "serial_monitor_attach_request.json"
        self._detach_request_file = DAEMON_DIR / "serial_monitor_detach_request.json"
        self._poll_request_file = DAEMON_DIR / "serial_monitor_poll_request.json"
        # Use per-client response file to prevent race conditions
        self._response_file = DAEMON_DIR / f"serial_monitor_response_{self.client_id}.json"
        self._preempt_file = DAEMON_DIR / f"serial_monitor_preempt_{port}.json"

        if self.verbose:
            logging.info(f"[SerialMonitor] Initialized for {port} @ {baud_rate} baud (client_id={self.client_id})")

    def __enter__(self) -> "SerialMonitor":
        """Attach to daemon serial session (context manager entry).

        Returns:
            Self for use in with statement

        Raises:
            RuntimeError: If attachment fails
        """
        self._attach()
        return self

    def __exit__(self, exc_type: type[BaseException] | None, exc_val: BaseException | None, exc_tb: Any) -> None:
        """Detach from daemon serial session (context manager exit).

        Args:
            exc_type: Exception type (if any)
            exc_val: Exception value (if any)
            exc_tb: Exception traceback (if any)
        """
        self._detach()

    def _attach(self) -> None:
        """Send attach request to daemon and wait for confirmation.

        Raises:
            RuntimeError: If attach fails or times out
        """
        if self._attached:
            if self.verbose:
                logging.warning("[SerialMonitor] Already attached, skipping attach")
            return

        # Ensure daemon is running before attempting attach
        ensure_daemon_running()  # Raises RuntimeError if daemon fails to start

        request = SerialMonitorAttachRequest(
            client_id=self.client_id,
            port=self.port,
            baud_rate=self.baud_rate,
            open_if_needed=True,
        )

        # Write attach request
        self._write_request_file(self._attach_request_file, request)

        # Wait for response
        response = self._wait_for_response(timeout=OPERATION_TIMEOUT)
        if not response or not response.success:
            raise RuntimeError(f"Failed to attach to {self.port}: {response.message if response else 'timeout'}")

        self._attached = True
        if self.verbose:
            logging.info(f"[SerialMonitor] Attached to {self.port}")

    def _detach(self) -> None:
        """Send detach request to daemon and wait for confirmation."""
        if not self._attached:
            return

        request = SerialMonitorDetachRequest(
            client_id=self.client_id,
            port=self.port,
        )

        # Write detach request and wait for confirmation
        try:
            self._write_request_file(self._detach_request_file, request)

            # Wait for response to ensure daemon has fully processed detach
            response = self._wait_for_response(timeout=OPERATION_TIMEOUT)
            if not response or not response.success:
                if self.verbose:
                    logging.warning(f"[SerialMonitor] Detach confirmation failed: {response.message if response else 'timeout'}")
            elif self.verbose:
                logging.info(f"[SerialMonitor] Detached from {self.port}")
        except KeyboardInterrupt:
            raise
        except Exception as e:
            if self.verbose:
                logging.warning(f"[SerialMonitor] Error during detach: {e}")

        self._attached = False

    def _poll_buffer(self, max_lines: int = 100) -> list[str]:
        """Poll daemon for new output lines.

        Uses incremental polling with last_index to avoid re-reading old lines.

        Args:
            max_lines: Maximum number of lines to fetch per poll

        Returns:
            List of new lines since last poll

        Raises:
            RuntimeError: If not attached or poll fails
        """
        if not self._attached:
            raise RuntimeError("Cannot poll buffer: not attached")

        request = SerialMonitorPollRequest(
            client_id=self.client_id,
            port=self.port,
            last_index=self._last_index,
            max_lines=max_lines,
        )

        # Write poll request
        self._write_request_file(self._poll_request_file, request)

        # Wait for response (short timeout for poll)
        response = self._wait_for_response(timeout=0.5)
        if not response:
            return []  # Timeout is normal during polling

        if not response.success:
            raise RuntimeError(f"Poll failed: {response.message}")

        # Update index for next poll
        self._last_index = response.current_index

        return response.lines

    def _check_preemption(self) -> bool:
        """Check if deploy has preempted this monitor.

        Returns:
            True if preempted, False otherwise
        """
        return self._preempt_file.exists()

    def _wait_for_reconnect(self) -> None:
        """Wait for deploy to complete and re-attach.

        Polls preemption file until it's deleted, then re-attaches.
        """
        if self.verbose:
            logging.info(f"[SerialMonitor] Deploy preempted {self.port}, waiting to reconnect...")

        # Poll until preemption file is deleted
        while self._preempt_file.exists():
            time.sleep(0.5)

        # Re-attach
        if self.verbose:
            logging.info(f"[SerialMonitor] Deploy completed, reconnecting to {self.port}...")

        self._attached = False  # Reset state
        self._attach()

    def read_lines(self, timeout: float | None = None) -> Iterator[str]:
        """Stream lines from serial port (blocking iterator).

        This is the main entry point for reading serial output. It polls the
        daemon every 100ms for new lines, invokes hooks, and handles preemption.

        Args:
            timeout: Maximum time to wait for lines (None = infinite)
                     Note: Timeout starts from first call, not from last line received

        Yields:
            Lines from serial output (as strings, without newlines)

        Raises:
            MonitorPreemptedException: If preempted and auto_reconnect=False
            MonitorHookError: If a hook raises an exception
            RuntimeError: If not attached or daemon error occurs
        """
        if not self._attached:
            raise RuntimeError("Cannot read lines: not attached")

        start_time = time.time()

        while True:
            # Check timeout
            if timeout is not None and (time.time() - start_time) > timeout:
                if self.verbose:
                    logging.info("[SerialMonitor] Timeout reached, stopping read_lines")
                return

            # Check for preemption
            if self._check_preemption():
                if self.auto_reconnect:
                    self._wait_for_reconnect()
                    # Reset timeout after reconnect (optional - user might want different behavior)
                    start_time = time.time()
                    continue
                else:
                    raise MonitorPreemptedException(self.port, "deploy_operation")

            # Poll for new lines
            try:
                lines = self._poll_buffer(max_lines=1000)
            except RuntimeError as e:
                if self.verbose:
                    logging.error(f"[SerialMonitor] Poll error: {e}")
                raise

            # Yield lines and invoke hooks
            for line in lines:
                self.last_line = line
                yield line

                # Invoke hooks (in order)
                for hook in self.hooks:
                    try:
                        hook(line)
                    except KeyboardInterrupt:
                        raise
                    except Exception as e:
                        raise MonitorHookError(hook, e) from e

            # Sleep before next poll
            time.sleep(POLL_INTERVAL)

    def write(self, data: str | bytes) -> int:
        """Write data to serial port.

        Uses existing SerialWriteRequest message (already in messages.py).
        Automatically acquires writer lock, writes, and releases.

        Args:
            data: String or bytes to write to serial port

        Returns:
            Number of bytes written

        Raises:
            RuntimeError: If not attached or write fails
        """
        if not self._attached:
            raise RuntimeError("Cannot write: not attached")

        # Convert string to bytes if needed
        if isinstance(data, str):
            data_bytes = data.encode("utf-8")
        else:
            data_bytes = data

        # Encode to base64 for JSON transport
        import base64

        data_b64 = base64.b64encode(data_bytes).decode("ascii")

        # Create write request (reuse existing message type)
        request = SerialWriteRequest(
            client_id=self.client_id,
            port=self.port,
            data=data_b64,
            acquire_writer=True,  # Auto-acquire writer lock
        )

        # Write request to serial_write_request.json (existing file)
        write_request_file = DAEMON_DIR / "serial_write_request.json"
        # Use per-client response file to prevent race conditions
        write_response_file = DAEMON_DIR / f"serial_write_response_{self.client_id}.json"

        self._write_request_file(write_request_file, request)

        # Wait for response
        response_data = self._wait_for_response_file(write_response_file, timeout=5.0)
        if not response_data:
            raise RuntimeError("Write request timeout")

        # Parse response (SerialSessionResponse)
        from fbuild.daemon.messages import SerialSessionResponse

        response = SerialSessionResponse.from_dict(response_data)
        if not response.success:
            raise RuntimeError(f"Write failed: {response.message}")

        return response.bytes_written

    def write_json_rpc(self, request: dict, timeout: float = 5.0) -> dict | None:
        """Send JSON-RPC request and wait for matching response.

        Useful for validation scripts that need to configure device state.
        Writes JSON request line, polls output for matching response ID.

        Args:
            request: JSON-RPC request dictionary (must have 'id' field)
            timeout: Maximum time to wait for response

        Returns:
            JSON-RPC response dictionary, or None if timeout

        Raises:
            ValueError: If request missing 'id' field
            RuntimeError: If not attached or write fails
        """
        if "id" not in request:
            raise ValueError("JSON-RPC request must have 'id' field")

        request_id = request["id"]

        # Send request
        request_json = json.dumps(request) + "\n"
        self.write(request_json)

        # Poll for matching response
        start_time = time.time()
        for line in self.read_lines(timeout=timeout):
            # Try to parse as JSON
            try:
                response = json.loads(line)
                if isinstance(response, dict) and response.get("id") == request_id:
                    return response
            except json.JSONDecodeError:
                continue  # Not JSON, keep polling

            # Check timeout
            if (time.time() - start_time) > timeout:
                return None

        return None

    def run_until(
        self,
        condition: Callable[[], bool],
        timeout: float | None = None,
    ) -> bool:
        """Read lines until condition() returns True or timeout.

        Useful for waiting for specific events or states.

        Args:
            condition: Function that returns True when done
            timeout: Maximum time to wait (None = infinite)

        Returns:
            True if condition met, False if timeout

        Example:
            >>> mon.run_until(lambda: 'READY' in mon.last_line, timeout=10.0)
        """
        start_time = time.time()

        for _ in self.read_lines(timeout=timeout):
            if condition():
                return True

            if timeout is not None and (time.time() - start_time) > timeout:
                return False

        return False

    def _write_request_file(self, file_path: Path, request: Any) -> None:
        """Atomically write request file.

        Args:
            file_path: Path to request file
            request: Request object with to_dict() method
        """
        DAEMON_DIR.mkdir(parents=True, exist_ok=True)

        # Atomic write using temporary file
        temp_file = file_path.with_suffix(".tmp")
        with open(temp_file, "w") as f:
            json.dump(request.to_dict(), f, indent=2)

        # Atomic rename
        temp_file.replace(file_path)

    def _wait_for_response(self, timeout: float = 5.0) -> SerialMonitorResponse | None:
        """Wait for response from daemon.

        Polls response file until it appears or timeout expires.

        Args:
            timeout: Maximum time to wait for response

        Returns:
            SerialMonitorResponse object, or None if timeout
        """
        response_data = self._wait_for_response_file(self._response_file, timeout=timeout)
        if not response_data:
            return None

        return SerialMonitorResponse.from_dict(response_data)

    def _wait_for_response_file(self, file_path: Path, timeout: float = 5.0) -> dict | None:
        """Wait for response file to appear and read it.

        Args:
            file_path: Path to response file
            timeout: Maximum time to wait

        Returns:
            Response dictionary, or None if timeout
        """
        start_time = time.time()
        poll_interval = 0.05  # 50ms polls for faster response

        while (time.time() - start_time) < timeout:
            if file_path.exists():
                try:
                    with open(file_path) as f:
                        data = json.load(f)

                    # Delete response file after reading
                    file_path.unlink(missing_ok=True)
                    return data

                except (json.JSONDecodeError, OSError):
                    # Corrupted or incomplete file, retry
                    time.sleep(poll_interval)
                    continue

            time.sleep(poll_interval)

        return None
