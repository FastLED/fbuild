"""Native async WebSocket-based SerialMonitor API.

This module provides an async-native SerialMonitor that works directly within
an existing asyncio event loop. Unlike the sync SerialMonitor which uses
run_until_complete() internally, this class exposes connect()/close()/write()/
read_line()/read_lines() as awaitable coroutines.

Key Features:
- Native async/await API — no run_until_complete(), safe in running event loops
- Pre-warms serial writer during connect() to eliminate first-write latency
- Both sync and async hook callbacks supported
- Async context manager support (async with)
- Same WebSocket protocol as sync SerialMonitor

Example Usage:
    >>> import asyncio
    >>> from fbuild.api import AsyncSerialMonitor
    >>>
    >>> async def main():
    ...     async with AsyncSerialMonitor(port='COM13', baud_rate=115200) as mon:
    ...         await mon.write("hello\\n")
    ...         async for line in mon.read_lines(timeout=30.0):
    ...             print(line)
    ...             if 'READY' in line:
    ...                 break
    >>>
    >>> asyncio.run(main())
"""

import asyncio
import base64
import inspect
import json
import logging
import time
import uuid
from collections.abc import AsyncIterator, Awaitable, Callable
from typing import Any

import websockets
from websockets.asyncio.client import ClientConnection

from fbuild.api.serial_monitor import MonitorHookError, MonitorPreemptedException
from fbuild.daemon.client.http_utils import get_daemon_port
from fbuild.daemon.client.lifecycle import ensure_daemon_running

# Hook type: callback that receives each line — sync or async
AsyncMonitorHook = Callable[[str], None] | Callable[[str], Awaitable[None]]

# Default timeout for attach/detach operations
OPERATION_TIMEOUT = 60.0


class AsyncSerialMonitor:
    """Async context manager for WebSocket-based daemon-routed serial monitoring.

    This class provides a native async API for monitoring serial output through
    the fbuild daemon using WebSocket communication. Unlike the sync SerialMonitor,
    this class is safe to use within an already-running asyncio event loop.

    The optional pre_acquire_writer flag pre-warms the serial writer during
    connect(), eliminating the ~25 second first-write latency caused by USB CDC
    retry logic.

    Example:
        >>> async with AsyncSerialMonitor(port='COM13', baud_rate=115200) as mon:
        ...     await mon.write("hello\\n")
        ...     async for line in mon.read_lines(timeout=30.0):
        ...         if 'READY' in line:
        ...             break

    Attributes:
        port: Serial port to monitor
        baud_rate: Baud rate for serial connection
        hooks: List of callback functions invoked for each line
        auto_reconnect: Whether to automatically reconnect after deploy preemption
        verbose: Whether to log verbose debug information
        client_id: Unique identifier for this monitor instance
        last_line: Most recent line received
    """

    def __init__(
        self,
        port: str,
        baud_rate: int = 115200,
        hooks: list[AsyncMonitorHook] | None = None,
        auto_reconnect: bool = True,
        verbose: bool = False,
        pre_acquire_writer: bool = False,
    ):
        """Initialize AsyncSerialMonitor.

        Args:
            port: Serial port to monitor (e.g., "COM13", "/dev/ttyUSB0")
            baud_rate: Baud rate for serial connection (default: 115200)
            hooks: Optional list of callback functions to invoke for each line.
                   Supports both sync and async callbacks.
            auto_reconnect: Whether to automatically reconnect after deploy preemption.
                            If True, monitoring pauses during deploy and resumes after.
                            If False, raises MonitorPreemptedException on preemption.
            verbose: Whether to log verbose debug information to console
            pre_acquire_writer: Whether to pre-acquire the serial writer during connect().
                                This eliminates ~25s first-write latency from USB CDC retry logic.
        """
        self.port = port
        self.baud_rate = baud_rate
        self.hooks = hooks or []
        self.auto_reconnect = auto_reconnect
        self.verbose = verbose
        self.pre_acquire_writer = pre_acquire_writer

        # Generate unique client ID
        self.client_id = f"async_serial_monitor_{uuid.uuid4().hex[:8]}"

        # Tracking state
        self._connected = False
        self._writer_pre_acquired = False
        self.last_line = ""

        # WebSocket connection
        self._ws: ClientConnection | None = None
        self._line_queue: asyncio.Queue[str] = asyncio.Queue()
        self._error_queue: asyncio.Queue[Exception] = asyncio.Queue()
        self._write_ack_queue: asyncio.Queue[dict[str, Any]] = asyncio.Queue()
        self._receiver_task: asyncio.Task[None] | None = None

        if self.verbose:
            logging.info(f"[AsyncSerialMonitor] Initialized for {port} @ {baud_rate} baud (client_id={self.client_id})")

    @property
    def is_connected(self) -> bool:
        """Whether the monitor is currently connected and attached."""
        return self._connected

    async def connect(self) -> None:
        """Connect to daemon and attach to serial port.

        Ensures the daemon is running, connects via WebSocket, sends the
        attach message, and starts the background receiver task. If
        pre_acquire_writer is True, also pre-warms the serial writer lock.

        Raises:
            RuntimeError: If connection or attachment fails
        """
        if self._connected:
            if self.verbose:
                logging.warning("[AsyncSerialMonitor] Already connected, skipping connect")
            return

        # Ensure daemon is running (sync call, offload to executor)
        loop = asyncio.get_event_loop()
        await loop.run_in_executor(None, ensure_daemon_running)

        # Get daemon port (sync call)
        daemon_port = await loop.run_in_executor(None, get_daemon_port)
        ws_url = f"ws://127.0.0.1:{daemon_port}/ws/serial-monitor"

        try:
            self._ws = await websockets.connect(ws_url)

            # Send attach request
            attach_msg: dict[str, Any] = {
                "type": "attach",
                "client_id": self.client_id,
                "port": self.port,
                "baud_rate": self.baud_rate,
                "open_if_needed": True,
            }
            if self.pre_acquire_writer:
                attach_msg["pre_acquire_writer"] = True

            await self._ws.send(json.dumps(attach_msg))

            # Wait for attach response
            response_text = await asyncio.wait_for(self._ws.recv(), timeout=OPERATION_TIMEOUT)
            response = json.loads(response_text)

            if response.get("type") != "attached" or not response.get("success"):
                raise RuntimeError(f"Failed to attach: {response.get('message', 'Unknown error')}")

            # Check if writer was pre-acquired
            self._writer_pre_acquired = response.get("writer_pre_acquired", False)

            # Start background receiver task
            self._receiver_task = asyncio.create_task(self._receive_messages())

            self._connected = True

            if self.verbose:
                logging.info(f"[AsyncSerialMonitor] Connected to {self.port} (writer_pre_acquired={self._writer_pre_acquired})")

        except KeyboardInterrupt:
            if self._ws:
                await self._ws.close()
                self._ws = None
            raise
        except RuntimeError:
            if self._ws:
                await self._ws.close()
                self._ws = None
            raise
        except Exception as e:
            if self._ws:
                await self._ws.close()
                self._ws = None
            raise RuntimeError(f"Failed to connect to {self.port}: {e}") from e

    async def close(self) -> None:
        """Detach from serial port and close WebSocket connection.

        Safe to call multiple times — no-op if already disconnected.
        """
        if not self._connected or not self._ws:
            return

        try:
            # Cancel receiver task first to avoid concurrent recv() calls
            if self._receiver_task:
                self._receiver_task.cancel()
                try:
                    await self._receiver_task
                except asyncio.CancelledError:
                    pass
                self._receiver_task = None

            # Send detach request
            detach_msg = {"type": "detach"}
            await self._ws.send(json.dumps(detach_msg))

            # Wait for detach confirmation
            try:
                response_text = await asyncio.wait_for(self._ws.recv(), timeout=5.0)
                response = json.loads(response_text)
                if self.verbose:
                    if response.get("type") == "detached" and response.get("success"):
                        logging.info(f"[AsyncSerialMonitor] Detached from {self.port}")
                    else:
                        logging.warning(f"[AsyncSerialMonitor] Detach failed: {response.get('message', 'Unknown')}")
            except asyncio.TimeoutError:
                if self.verbose:
                    logging.warning("[AsyncSerialMonitor] Detach confirmation timeout")

        except KeyboardInterrupt:
            raise
        except Exception as e:
            if self.verbose:
                logging.warning(f"[AsyncSerialMonitor] Error during close: {e}")
        finally:
            # Ensure receiver task is cancelled
            if self._receiver_task:
                self._receiver_task.cancel()
                try:
                    await self._receiver_task
                except asyncio.CancelledError:
                    pass
                self._receiver_task = None

            # Close WebSocket
            if self._ws:
                await self._ws.close()
                self._ws = None

            self._connected = False
            self._writer_pre_acquired = False

    async def __aenter__(self) -> "AsyncSerialMonitor":
        """Async context manager entry — calls connect().

        Returns:
            Self for use in async with statement
        """
        await self.connect()
        return self

    async def __aexit__(self, exc_type: type[BaseException] | None, exc_val: BaseException | None, exc_tb: Any) -> None:
        """Async context manager exit — calls close().

        Args:
            exc_type: Exception type (if any)
            exc_val: Exception value (if any)
            exc_tb: Exception traceback (if any)
        """
        await self.close()

    async def write(self, data: str | bytes) -> int:
        """Write data to serial port.

        Args:
            data: String or bytes to write to serial port

        Returns:
            Number of bytes written

        Raises:
            RuntimeError: If not connected or write fails/times out
        """
        if not self._connected or not self._ws:
            raise RuntimeError("Cannot write: not connected")

        # Convert string to bytes if needed
        if isinstance(data, str):
            data_bytes = data.encode("utf-8")
        else:
            data_bytes = data

        # Encode to base64 for JSON transport
        data_b64 = base64.b64encode(data_bytes).decode("ascii")

        # Send write request
        write_msg: dict[str, Any] = {
            "type": "write",
            "data": data_b64,
        }
        await self._ws.send(json.dumps(write_msg))

        # Wait for write_ack or error
        try:
            ack_task: asyncio.Task[dict[str, Any]] = asyncio.create_task(self._write_ack_queue.get())
            error_task: asyncio.Task[Exception] = asyncio.create_task(self._error_queue.get())

            done, pending = await asyncio.wait(
                [ack_task, error_task],
                timeout=25.0,
                return_when=asyncio.FIRST_COMPLETED,
            )

            # Cancel pending tasks
            for task in pending:
                task.cancel()
                try:
                    await task
                except asyncio.CancelledError:
                    pass

            if not done:
                raise RuntimeError("Write acknowledgement timeout")

            # Check which completed
            if error_task in done:
                error = error_task.result()
                raise error

            response: dict[str, Any] = ack_task.result()

        except asyncio.TimeoutError:
            raise RuntimeError("Write acknowledgement timeout") from None

        if response.get("type") != "write_ack" or not response.get("success"):
            raise RuntimeError(f"Write failed: {response.get('message', 'Unknown error')}")

        return response.get("bytes_written", 0)

    async def read_line(self, timeout: float | None = None) -> str | None:
        """Read a single line from serial output.

        Args:
            timeout: Maximum time to wait for a line (None = block indefinitely)

        Returns:
            Line string, or None if timeout

        Raises:
            RuntimeError: If not connected or daemon error occurs
            MonitorPreemptedException: If preempted and auto_reconnect=False
        """
        if not self._connected:
            raise RuntimeError("Cannot read: not connected")

        # Check for errors first
        if not self._error_queue.empty():
            try:
                error = self._error_queue.get_nowait()
                raise error
            except asyncio.QueueEmpty:
                pass

        try:
            if timeout is not None:
                line = await asyncio.wait_for(self._line_queue.get(), timeout=timeout)
            else:
                line = await self._line_queue.get()
            self.last_line = line
            return line
        except asyncio.TimeoutError:
            return None

    async def read_lines(self, timeout: float | None = None) -> AsyncIterator[str]:
        """Stream lines from serial port as an async iterator.

        Yields lines in real-time, invokes hooks for each line, and handles
        preemption. Stops when timeout is reached.

        Args:
            timeout: Maximum time to yield lines (None = infinite)

        Yields:
            Lines from serial output (as strings, without newlines)

        Raises:
            MonitorPreemptedException: If preempted and auto_reconnect=False
            MonitorHookError: If a hook raises an exception
            RuntimeError: If not connected or daemon error occurs
        """
        if not self._connected:
            raise RuntimeError("Cannot read lines: not connected")

        start_time = time.time()

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
                    logging.info("[AsyncSerialMonitor] Timeout reached, stopping read_lines")
                return

            # Calculate remaining timeout
            if timeout is not None:
                remaining = timeout - (time.time() - start_time)
                if remaining <= 0:
                    return
            else:
                remaining = None

            # Read next line
            try:
                if remaining is not None:
                    line = await asyncio.wait_for(self._line_queue.get(), timeout=remaining)
                else:
                    line = await self._line_queue.get()
            except asyncio.TimeoutError:
                return

            # Update last_line and yield
            self.last_line = line
            yield line

            # Invoke hooks (in order)
            for hook in self.hooks:
                try:
                    if inspect.iscoroutinefunction(hook):
                        await hook(line)
                    else:
                        hook(line)
                except KeyboardInterrupt:
                    raise
                except Exception as e:
                    raise MonitorHookError(hook, e) from e  # type: ignore[arg-type]

    async def write_json_rpc(self, request: dict[str, Any], timeout: float) -> dict[str, Any] | None:
        """Send JSON-RPC request and wait for matching response.

        Writes a JSON request line, then reads serial output for a matching
        response with the same 'id' field.

        Args:
            request: JSON-RPC request dictionary (must have 'id' field)
            timeout: Maximum time to wait for response

        Returns:
            JSON-RPC response dictionary, or None if timeout

        Raises:
            ValueError: If request missing 'id' field
            RuntimeError: If not connected or write fails
        """
        if "id" not in request:
            raise ValueError("JSON-RPC request must have 'id' field")

        request_id = request["id"]

        # Send request
        request_json = json.dumps(request) + "\n"
        await self.write(request_json)

        # Read lines looking for matching response
        start_time = time.time()
        async for line in self.read_lines(timeout=timeout):
            try:
                response = json.loads(line)
                if isinstance(response, dict) and response.get("id") == request_id:
                    return response
            except json.JSONDecodeError:
                continue

            if (time.time() - start_time) > timeout:
                return None

        return None

    async def run_until(
        self,
        condition: Callable[[], bool],
        timeout: float | None = None,
    ) -> bool:
        """Read lines until condition() returns True or timeout.

        Args:
            condition: Function that returns True when done
            timeout: Maximum time to wait (None = infinite)

        Returns:
            True if condition met, False if timeout

        Example:
            >>> await mon.run_until(lambda: 'READY' in mon.last_line, timeout=10.0)
        """
        async for _ in self.read_lines(timeout=timeout):
            if condition():
                return True

        return False

    async def _receive_messages(self) -> None:
        """Background task to receive messages from WebSocket and route to queues."""
        try:
            while self._ws:
                try:
                    message_text = await self._ws.recv()
                    message = json.loads(message_text)
                    msg_type = message.get("type")

                    if msg_type == "data":
                        lines = message.get("lines", [])
                        for line in lines:
                            await self._line_queue.put(line)

                    elif msg_type == "write_ack":
                        await self._write_ack_queue.put(message)

                    elif msg_type == "preempted":
                        if self.auto_reconnect:
                            if self.verbose:
                                logging.info("[AsyncSerialMonitor] Preempted, waiting for reconnect...")
                        else:
                            preempted_by = message.get("preempted_by", "unknown")
                            exc = MonitorPreemptedException(self.port, preempted_by)
                            await self._error_queue.put(exc)

                    elif msg_type == "reconnected":
                        if self.verbose:
                            logging.info(f"[AsyncSerialMonitor] Reconnected to {self.port}")

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
