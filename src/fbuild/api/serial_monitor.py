"""WebSocket-based SerialMonitor API - Real-time daemon-routed serial I/O.

This module provides the SerialMonitor context manager using WebSocket
communication instead of file-based IPC. This eliminates polling overhead
and provides real-time data delivery.

Key Features:
- Real-time data streaming (no 100ms polling delay)
- Multiple clients can monitor the same port concurrently
- Deploy operations can preempt monitors gracefully (auto-reconnect)
- No OS-level port locks held by client processes
- Simple context manager interface (compatible with file-based API)

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
    validate.py (client) → SerialMonitor API → WebSocket → Daemon (SharedSerialManager) → COM13

WebSocket Protocol:
    Client → Server:
        {"type": "attach", "client_id": "...", "port": "COM13", "baud_rate": 115200}
        {"type": "write", "data": "base64_encoded_data"}
        {"type": "detach"}

    Server → Client:
        {"type": "attached", "success": true, "message": "..."}
        {"type": "data", "lines": ["line1", "line2"], "current_index": 42}
        {"type": "preempted", "reason": "deploy"}
        {"type": "reconnected"}
        {"type": "write_ack", "bytes_written": 10}
        {"type": "error", "message": "..."}
"""

import asyncio
import base64
import json
import logging
import time
import uuid
from collections.abc import Callable, Iterator
from typing import Any

import websockets
from websockets.asyncio.client import ClientConnection

from fbuild.daemon.client.http_utils import get_daemon_port
from fbuild.daemon.client.lifecycle import ensure_daemon_running

# Hook type: callback function that receives each line
MonitorHook = Callable[[str], None]

# Default timeout for attach/detach operations
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
    """Context manager for WebSocket-based daemon-routed serial monitoring.

    This class provides a high-level API for monitoring serial output through
    the fbuild daemon using WebSocket communication. It handles attachment,
    real-time data streaming, preemption, and cleanup automatically.

    The monitor uses WebSocket to communicate with the daemon's
    SharedSerialManager. This eliminates file-based polling and provides
    real-time data delivery.

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
        self.last_line = ""  # Most recent line (for hooks/conditions)

        # WebSocket connection
        self._ws: ClientConnection | None = None
        self._event_loop: asyncio.AbstractEventLoop | None = None
        self._line_queue: asyncio.Queue[str] = asyncio.Queue()
        self._error_queue: asyncio.Queue[Exception] = asyncio.Queue()
        self._write_ack_queue: asyncio.Queue[dict] = asyncio.Queue()  # For write acknowledgements
        self._receiver_task: asyncio.Task | None = None

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
        """Connect to WebSocket and send attach request.

        Raises:
            RuntimeError: If attach fails or times out
        """
        if self._attached:
            if self.verbose:
                logging.warning("[SerialMonitor] Already attached, skipping attach")
            return

        # Ensure daemon is running
        ensure_daemon_running()

        # Get daemon port
        daemon_port = get_daemon_port()
        ws_url = f"ws://127.0.0.1:{daemon_port}/ws/serial-monitor"

        # Create event loop for async operations
        try:
            self._event_loop = asyncio.get_event_loop()
        except RuntimeError:
            # No event loop in current thread, create one
            self._event_loop = asyncio.new_event_loop()
            asyncio.set_event_loop(self._event_loop)

        # Connect to WebSocket and attach
        async def _connect_and_attach():
            try:
                self._ws = await websockets.connect(ws_url)

                # Send attach request
                attach_msg = {
                    "type": "attach",
                    "client_id": self.client_id,
                    "port": self.port,
                    "baud_rate": self.baud_rate,
                    "open_if_needed": True,
                }
                await self._ws.send(json.dumps(attach_msg))

                # Wait for attach response
                response_text = await asyncio.wait_for(self._ws.recv(), timeout=OPERATION_TIMEOUT)
                response = json.loads(response_text)

                if response.get("type") != "attached" or not response.get("success"):
                    raise RuntimeError(f"Failed to attach: {response.get('message', 'Unknown error')}")

                # Start background receiver task
                self._receiver_task = asyncio.create_task(self._receive_messages())

                if self.verbose:
                    logging.info(f"[SerialMonitor] Attached to {self.port}")

            except KeyboardInterrupt:
                if self._ws:
                    await self._ws.close()
                    self._ws = None
                raise
            except Exception as e:
                if self._ws:
                    await self._ws.close()
                    self._ws = None
                raise RuntimeError(f"Failed to attach to {self.port}: {e}") from e

        self._event_loop.run_until_complete(_connect_and_attach())
        self._attached = True

    def _detach(self) -> None:
        """Send detach request and close WebSocket connection."""
        if not self._attached or not self._ws:
            return

        async def _detach_and_close():
            try:
                # Cancel receiver task FIRST to avoid concurrent recv() calls
                if self._receiver_task:
                    self._receiver_task.cancel()
                    try:
                        await self._receiver_task
                    except asyncio.CancelledError:
                        pass
                    self._receiver_task = None

                # Now we can safely send detach and receive confirmation
                # Send detach request
                detach_msg = {"type": "detach"}
                await self._ws.send(json.dumps(detach_msg))  # type: ignore

                # Wait for detach confirmation (with timeout)
                try:
                    response_text = await asyncio.wait_for(self._ws.recv(), timeout=5.0)  # type: ignore
                    response = json.loads(response_text)
                    if self.verbose:
                        if response.get("type") == "detached" and response.get("success"):
                            logging.info(f"[SerialMonitor] Detached from {self.port}")
                        else:
                            logging.warning(f"[SerialMonitor] Detach failed: {response.get('message', 'Unknown')}")
                except asyncio.TimeoutError:
                    if self.verbose:
                        logging.warning("[SerialMonitor] Detach confirmation timeout")

            except KeyboardInterrupt:
                raise
            except Exception as e:
                if self.verbose:
                    logging.warning(f"[SerialMonitor] Error during detach: {e}")
            finally:
                # Ensure receiver task is cancelled (in case of exceptions above)
                if self._receiver_task:
                    self._receiver_task.cancel()
                    try:
                        await self._receiver_task
                    except asyncio.CancelledError:
                        pass

                # Close WebSocket
                if self._ws:
                    await self._ws.close()
                    self._ws = None

        if self._event_loop:
            self._event_loop.run_until_complete(_detach_and_close())

        self._attached = False

    async def _receive_messages(self) -> None:
        """Background task to receive messages from WebSocket and queue lines."""
        try:
            while self._ws:
                try:
                    message_text = await self._ws.recv()
                    message = json.loads(message_text)
                    msg_type = message.get("type")

                    if msg_type == "data":
                        # Queue lines for read_lines()
                        lines = message.get("lines", [])
                        for line in lines:
                            await self._line_queue.put(line)

                    elif msg_type == "write_ack":
                        # Route write acknowledgements to write queue
                        await self._write_ack_queue.put(message)

                    elif msg_type == "preempted":
                        # Handle preemption
                        if self.auto_reconnect:
                            if self.verbose:
                                logging.info("[SerialMonitor] Preempted, waiting for reconnect...")
                        else:
                            # Queue exception
                            preempted_by = message.get("preempted_by", "unknown")
                            exc = MonitorPreemptedException(self.port, preempted_by)
                            await self._error_queue.put(exc)

                    elif msg_type == "reconnected":
                        if self.verbose:
                            logging.info(f"[SerialMonitor] Reconnected to {self.port}")

                    elif msg_type == "error":
                        error_msg = message.get("message", "Unknown error")
                        exc = RuntimeError(f"Monitor error: {error_msg}")
                        await self._error_queue.put(exc)

                except websockets.exceptions.ConnectionClosed:
                    break
                except KeyboardInterrupt:
                    raise
                except Exception as e:
                    await self._error_queue.put(e)
                    break

        except asyncio.CancelledError:
            pass

    def read_lines(self, timeout: float | None = None) -> Iterator[str]:
        """Stream lines from serial port (blocking iterator).

        This is the main entry point for reading serial output. It receives
        lines pushed from the WebSocket in real-time, invokes hooks, and
        handles preemption.

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

        async def _read_line_async():
            """Read one line from queue with timeout."""
            if timeout is not None:
                remaining = timeout - (time.time() - start_time)
                if remaining <= 0:
                    return None
                try:
                    return await asyncio.wait_for(self._line_queue.get(), timeout=remaining)
                except asyncio.TimeoutError:
                    return None
            else:
                return await self._line_queue.get()

        while True:
            # Check for errors from receiver task
            if not self._error_queue.empty():
                try:
                    error = self._error_queue.get_nowait()
                    raise error
                except asyncio.QueueEmpty:
                    pass

            # Check timeout
            if timeout is not None and (time.time() - start_time) > timeout:
                if self.verbose:
                    logging.info("[SerialMonitor] Timeout reached, stopping read_lines")
                return

            # Read next line (async)
            if not self._event_loop:
                raise RuntimeError("Event loop not initialized")

            line = self._event_loop.run_until_complete(_read_line_async())
            if line is None:
                # Timeout occurred
                return

            # Update last_line and yield
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

    def write(self, data: str | bytes) -> int:
        """Write data to serial port.

        Args:
            data: String or bytes to write to serial port

        Returns:
            Number of bytes written

        Raises:
            RuntimeError: If not attached or write fails
        """
        if not self._attached or not self._ws:
            raise RuntimeError("Cannot write: not attached")

        # Convert string to bytes if needed
        if isinstance(data, str):
            data_bytes = data.encode("utf-8")
        else:
            data_bytes = data

        # Encode to base64 for JSON transport
        data_b64 = base64.b64encode(data_bytes).decode("ascii")

        # Send write request
        async def _write_async():
            write_msg = {
                "type": "write",
                "data": data_b64,
            }
            await self._ws.send(json.dumps(write_msg))  # type: ignore

            # Wait for write_ack via queue (receiver task routes it there)
            # This avoids concurrent recv() calls on the WebSocket
            try:
                response = await asyncio.wait_for(self._write_ack_queue.get(), timeout=5.0)
            except asyncio.TimeoutError:
                raise RuntimeError("Write acknowledgement timeout") from None

            if response.get("type") != "write_ack" or not response.get("success"):
                raise RuntimeError(f"Write failed: {response.get('message', 'Unknown error')}")

            return response.get("bytes_written", 0)

        if not self._event_loop:
            raise RuntimeError("Event loop not initialized")

        return self._event_loop.run_until_complete(_write_async())

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
