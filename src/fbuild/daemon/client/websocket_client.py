"""
WebSocket client utilities for fbuild CLI.

This module provides simple WebSocket client functions for consuming
real-time updates from the daemon WebSocket endpoints.

Features:
- Status updates stream during builds/deploys
- Serial monitor streaming
- Log streaming
- Automatic reconnection on connection loss
"""

from __future__ import annotations

import _thread
import asyncio
import json
import logging
import subprocess
import sys
from typing import Any, Callable

from fbuild.subprocess_utils import safe_run

try:
    import websockets
    from websockets.exceptions import ConnectionClosed, WebSocketException

    WEBSOCKETS_AVAILABLE = True
except ImportError:
    websockets = None
    WEBSOCKETS_AVAILABLE = False
    ConnectionClosed = Exception
    WebSocketException = Exception


def check_websockets_available() -> bool:
    """Check if websockets library is available.

    Returns:
        True if websockets library is installed, False otherwise
    """
    return WEBSOCKETS_AVAILABLE


def install_websockets() -> bool:
    """Attempt to install websockets library.

    Returns:
        True if installation succeeded, False otherwise
    """
    if WEBSOCKETS_AVAILABLE:
        return True

    try:
        safe_run(
            [sys.executable, "-m", "pip", "install", "websockets"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=True,
        )
        return True
    except KeyboardInterrupt:
        raise
    except Exception:
        return False


async def stream_status_updates(
    port: int,
    callback: Callable[[dict[str, Any]], None],
    stop_event: asyncio.Event | None = None,
) -> None:
    """Stream status updates from the daemon WebSocket.

    This function connects to the /ws/status endpoint and calls the callback
    function for each status update received.

    Args:
        port: Daemon HTTP port
        callback: Function to call with each status update
        stop_event: Optional event to signal when to stop streaming

    Example:
        >>> def handle_status(status: dict[str, Any]) -> None:
        >>>     print(f"Status: {status['state']} - {status['message']}")
        >>>
        >>> await stream_status_updates(8766, handle_status)
    """
    if not WEBSOCKETS_AVAILABLE:
        raise RuntimeError("websockets library not installed. Run: pip install websockets")

    uri = f"ws://127.0.0.1:{port}/ws/status"

    try:
        if websockets is None:
            raise RuntimeError("websockets library not available")
        async with websockets.connect(uri) as websocket:
            while True:
                # Check stop event
                if stop_event and stop_event.is_set():
                    break

                try:
                    # Receive message with timeout
                    response = await asyncio.wait_for(websocket.recv(), timeout=1.0)
                    data = json.loads(response)

                    # Call callback with status update
                    if data.get("type") in ("status", "pong"):
                        callback(data)

                except asyncio.TimeoutError:
                    # No message received, continue
                    continue

    except ConnectionClosed:
        logging.debug("Status WebSocket connection closed")
    except WebSocketException as e:
        logging.error(f"WebSocket error: {e}")
        raise


async def stream_monitor_data(
    port: int,
    session_id: str,
    data_callback: Callable[[str], None],
    write_queue: asyncio.Queue[str] | None = None,
    stop_event: asyncio.Event | None = None,
) -> None:
    """Stream serial monitor data via WebSocket.

    This function provides bidirectional communication with a serial device:
    - Calls data_callback for each line received from the device
    - Sends data from write_queue to the device

    Args:
        port: Daemon HTTP port
        session_id: Monitor session ID
        data_callback: Function to call with received data
        write_queue: Optional queue for sending data to device
        stop_event: Optional event to signal when to stop streaming

    Example:
        >>> def handle_data(data: str) -> None:
        >>>     print(data, end='')
        >>>
        >>> queue = asyncio.Queue()
        >>> await stream_monitor_data(8766, "session123", handle_data, queue)
    """
    if not WEBSOCKETS_AVAILABLE:
        raise RuntimeError("websockets library not installed. Run: pip install websockets")

    uri = f"ws://127.0.0.1:{port}/ws/monitor/{session_id}"

    try:
        if websockets is None:
            raise RuntimeError("websockets library not available")
        async with websockets.connect(uri) as websocket:
            # Task for receiving data
            async def receive_loop() -> None:
                while True:
                    if stop_event and stop_event.is_set():
                        break

                    try:
                        response = await asyncio.wait_for(websocket.recv(), timeout=0.5)
                        data = json.loads(response)

                        if data.get("type") == "monitor_data":
                            data_callback(data.get("data", ""))

                    except asyncio.TimeoutError:
                        continue

            # Task for sending data
            async def send_loop() -> None:
                if not write_queue:
                    return

                while True:
                    if stop_event and stop_event.is_set():
                        break

                    try:
                        write_data = await asyncio.wait_for(write_queue.get(), timeout=0.5)
                        await websocket.send(json.dumps({"type": "write", "data": write_data}))
                    except asyncio.TimeoutError:
                        continue

            # Run both tasks concurrently
            await asyncio.gather(receive_loop(), send_loop())

    except ConnectionClosed:
        logging.debug("Monitor WebSocket connection closed")
    except WebSocketException as e:
        logging.error(f"WebSocket error: {e}")
        raise


async def stream_logs(
    port: int,
    callback: Callable[[dict[str, Any]], None],
    stop_event: asyncio.Event | None = None,
) -> None:
    """Stream daemon logs via WebSocket.

    Args:
        port: Daemon HTTP port
        callback: Function to call with each log entry
        stop_event: Optional event to signal when to stop streaming

    Example:
        >>> def handle_log(entry: dict[str, Any]) -> None:
        >>>     print(f"[{entry['level']}] {entry['message']}")
        >>>
        >>> await stream_logs(8766, handle_log)
    """
    if not WEBSOCKETS_AVAILABLE:
        raise RuntimeError("websockets library not installed. Run: pip install websockets")

    uri = f"ws://127.0.0.1:{port}/ws/logs"

    try:
        if websockets is None:
            raise RuntimeError("websockets library not available")
        async with websockets.connect(uri) as websocket:
            while True:
                if stop_event and stop_event.is_set():
                    break

                try:
                    response = await asyncio.wait_for(websocket.recv(), timeout=1.0)
                    data = json.loads(response)

                    if data.get("type") == "log":
                        callback(data)

                except asyncio.TimeoutError:
                    continue

    except ConnectionClosed:
        logging.debug("Logs WebSocket connection closed")
    except WebSocketException as e:
        logging.error(f"WebSocket error: {e}")
        raise


# Synchronous wrappers for CLI usage


def stream_status_updates_sync(
    port: int,
    callback: Callable[[dict[str, Any]], None],
    timeout: float | None = None,
) -> None:
    """Synchronous wrapper for stream_status_updates.

    Args:
        port: Daemon HTTP port
        callback: Function to call with each status update
        timeout: Optional timeout in seconds
    """
    if not WEBSOCKETS_AVAILABLE:
        raise RuntimeError("websockets library not installed. Run: pip install websockets")

    stop_event = asyncio.Event()

    async def run() -> None:
        if timeout:
            await asyncio.wait_for(stream_status_updates(port, callback, stop_event), timeout=timeout)
        else:
            await stream_status_updates(port, callback, stop_event)

    try:
        asyncio.run(run())
    except KeyboardInterrupt:
        stop_event.set()
        _thread.interrupt_main()


def stream_monitor_data_sync(
    port: int,
    session_id: str,
    data_callback: Callable[[str], None],
    timeout: float | None = None,
) -> None:
    """Synchronous wrapper for stream_monitor_data.

    Args:
        port: Daemon HTTP port
        session_id: Monitor session ID
        data_callback: Function to call with received data
        timeout: Optional timeout in seconds
    """
    if not WEBSOCKETS_AVAILABLE:
        raise RuntimeError("websockets library not installed. Run: pip install websockets")

    stop_event = asyncio.Event()

    async def run() -> None:
        if timeout:
            await asyncio.wait_for(stream_monitor_data(port, session_id, data_callback, None, stop_event), timeout=timeout)
        else:
            await stream_monitor_data(port, session_id, data_callback, None, stop_event)

    try:
        asyncio.run(run())
    except KeyboardInterrupt:
        stop_event.set()
        _thread.interrupt_main()


def stream_logs_sync(
    port: int,
    callback: Callable[[dict[str, Any]], None],
    timeout: float | None = None,
) -> None:
    """Synchronous wrapper for stream_logs.

    Args:
        port: Daemon HTTP port
        callback: Function to call with each log entry
        timeout: Optional timeout in seconds
    """
    if not WEBSOCKETS_AVAILABLE:
        raise RuntimeError("websockets library not installed. Run: pip install websockets")

    stop_event = asyncio.Event()

    async def run() -> None:
        if timeout:
            await asyncio.wait_for(stream_logs(port, callback, stop_event), timeout=timeout)
        else:
            await stream_logs(port, callback, stop_event)

    try:
        asyncio.run(run())
    except KeyboardInterrupt:
        stop_event.set()
        _thread.interrupt_main()
