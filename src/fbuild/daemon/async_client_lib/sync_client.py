"""
Synchronous client wrapper for async daemon client.

This module provides a synchronous API that wraps the AsyncDaemonClient
by running operations in a dedicated event loop thread.
"""

from __future__ import annotations

import asyncio
import threading
from concurrent.futures import ThreadPoolExecutor
from typing import Any, Callable, Coroutine

from .client import (
    DEFAULT_HEARTBEAT_INTERVAL,
    DEFAULT_HOST,
    DEFAULT_PORT,
    DEFAULT_REQUEST_TIMEOUT,
    AsyncDaemonClient,
)


class SyncDaemonClient:
    """Synchronous wrapper around AsyncDaemonClient for use from sync code.

    This class provides a synchronous API that internally uses the async client
    by running operations in a dedicated event loop thread.

    Example:
        >>> client = SyncDaemonClient()
        >>> client.connect("localhost", 8765)
        >>> lock_acquired = client.acquire_lock("/project", "esp32", "/dev/ttyUSB0")
        >>> print(f"Lock acquired: {lock_acquired}")
        >>> client.disconnect()
    """

    def __init__(
        self,
        client_id: str | None = None,
        heartbeat_interval: float = DEFAULT_HEARTBEAT_INTERVAL,
        request_timeout: float = DEFAULT_REQUEST_TIMEOUT,
        auto_reconnect: bool = True,
    ) -> None:
        """Initialize the sync daemon client.

        Args:
            client_id: Unique client identifier (auto-generated if None)
            heartbeat_interval: Interval between heartbeats in seconds
            request_timeout: Default timeout for requests in seconds
            auto_reconnect: Whether to automatically reconnect on disconnect
        """
        self._async_client = AsyncDaemonClient(
            client_id=client_id,
            heartbeat_interval=heartbeat_interval,
            request_timeout=request_timeout,
            auto_reconnect=auto_reconnect,
        )
        self._loop: asyncio.AbstractEventLoop | None = None
        self._thread_executor = ThreadPoolExecutor(max_workers=1)
        self._loop_thread_started = False

    def __enter__(self) -> "SyncDaemonClient":
        """Context manager entry."""
        return self

    def __exit__(
        self,
        _exc_type: type | None,
        _exc_val: Exception | None,
        _exc_tb: Any,
    ) -> None:
        """Context manager exit."""
        self.disconnect()
        self.close()

    @property
    def client_id(self) -> str:
        """Get the client ID."""
        return self._async_client.client_id

    @property
    def is_connected(self) -> bool:
        """Check if client is connected."""
        return self._async_client.is_connected

    def _ensure_loop(self) -> asyncio.AbstractEventLoop:
        """Ensure event loop is running and return it."""
        if self._loop is None or not self._loop.is_running():
            self._loop = asyncio.new_event_loop()
            # Start loop in background thread
            self._loop_thread = threading.Thread(
                target=self._run_event_loop,
                daemon=True,
            )
            self._loop_thread.start()
            self._loop_thread_started = True

        return self._loop

    def _run_event_loop(self) -> None:
        """Run the event loop in a background thread."""
        if self._loop:
            asyncio.set_event_loop(self._loop)
            self._loop.run_forever()

    def _run_async(self, coro: Coroutine[Any, Any, Any]) -> Any:
        """Run an async coroutine from sync code.

        Args:
            coro: Coroutine to run

        Returns:
            Result of the coroutine
        """
        loop = self._ensure_loop()
        future = asyncio.run_coroutine_threadsafe(coro, loop)
        return future.result()

    def connect(
        self,
        host: str = DEFAULT_HOST,
        port: int = DEFAULT_PORT,
        timeout: float = 10.0,
    ) -> None:
        """Connect to the daemon server.

        Args:
            host: Daemon host address
            port: Daemon port number
            timeout: Connection timeout in seconds
        """
        self._run_async(self._async_client.connect(host, port, timeout))

    def disconnect(self, reason: str = "client requested") -> None:
        """Disconnect from the daemon server.

        Args:
            reason: Reason for disconnection
        """
        try:
            self._run_async(self._async_client.disconnect(reason))
        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception:
            pass

    def close(self) -> None:
        """Close the client and cleanup resources."""
        if self._loop and self._loop.is_running():
            self._loop.call_soon_threadsafe(self._loop.stop)

        self._thread_executor.shutdown(wait=False)

    def wait_for_connection(self, timeout: float = 30.0) -> None:
        """Wait for connection to be established.

        Args:
            timeout: Maximum time to wait
        """
        self._run_async(self._async_client.wait_for_connection(timeout))

    # =========================================================================
    # Lock Management
    # =========================================================================

    def acquire_lock(
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
            timeout: Maximum time to wait for the lock
            description: Human-readable description

        Returns:
            True if lock was acquired
        """
        return self._run_async(self._async_client.acquire_lock(project_dir, environment, port, lock_type, timeout, description))

    def release_lock(
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
            True if lock was released
        """
        return self._run_async(self._async_client.release_lock(project_dir, environment, port))

    def get_lock_status(
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
        return self._run_async(self._async_client.get_lock_status(project_dir, environment, port))

    def subscribe_lock_changes(
        self,
        callback: Callable[[dict[str, Any]], None],
        filter_key: str | None = None,
    ) -> str:
        """Subscribe to lock change events.

        Args:
            callback: Function to call when lock changes
            filter_key: Optional key to filter events

        Returns:
            Subscription ID
        """
        return self._run_async(self._async_client.subscribe_lock_changes(callback, filter_key))

    # =========================================================================
    # Firmware Queries
    # =========================================================================

    def query_firmware(
        self,
        port: str,
        source_hash: str,
        build_flags_hash: str | None = None,
    ) -> dict[str, Any]:
        """Query if firmware is current on a device.

        Args:
            port: Serial port of the device
            source_hash: Hash of the source files
            build_flags_hash: Hash of build flags

        Returns:
            Dictionary with firmware status
        """
        return self._run_async(self._async_client.query_firmware(port, source_hash, build_flags_hash))

    def subscribe_firmware_changes(
        self,
        callback: Callable[[dict[str, Any]], None],
        port: str | None = None,
    ) -> str:
        """Subscribe to firmware change events.

        Args:
            callback: Function to call when firmware changes
            port: Optional port to filter events

        Returns:
            Subscription ID
        """
        return self._run_async(self._async_client.subscribe_firmware_changes(callback, port))

    # =========================================================================
    # Serial Session Management
    # =========================================================================

    def attach_serial(
        self,
        port: str,
        baud_rate: int = 115200,
        as_reader: bool = True,
    ) -> bool:
        """Attach to a serial session.

        Args:
            port: Serial port to attach to
            baud_rate: Baud rate for the connection
            as_reader: Whether to attach as reader

        Returns:
            True if attached successfully
        """
        return self._run_async(self._async_client.attach_serial(port, baud_rate, as_reader))

    def detach_serial(
        self,
        port: str,
        close_port: bool = False,
    ) -> bool:
        """Detach from a serial session.

        Args:
            port: Serial port to detach from
            close_port: Whether to close port if last reader

        Returns:
            True if detached successfully
        """
        return self._run_async(self._async_client.detach_serial(port, close_port))

    def acquire_writer(
        self,
        port: str,
        timeout: float = 10.0,
    ) -> bool:
        """Acquire write access to a serial port.

        Args:
            port: Serial port
            timeout: Maximum time to wait

        Returns:
            True if write access acquired
        """
        return self._run_async(self._async_client.acquire_writer(port, timeout))

    def release_writer(self, port: str) -> bool:
        """Release write access to a serial port.

        Args:
            port: Serial port

        Returns:
            True if released
        """
        return self._run_async(self._async_client.release_writer(port))

    def write_serial(
        self,
        port: str,
        data: bytes,
        acquire_writer: bool = True,
    ) -> int:
        """Write data to a serial port.

        Args:
            port: Serial port
            data: Bytes to write
            acquire_writer: Whether to auto-acquire writer

        Returns:
            Number of bytes written
        """
        return self._run_async(self._async_client.write_serial(port, data, acquire_writer))

    def read_buffer(
        self,
        port: str,
        max_lines: int = 100,
    ) -> list[str]:
        """Read buffered serial output.

        Args:
            port: Serial port
            max_lines: Maximum lines to return

        Returns:
            List of output lines
        """
        return self._run_async(self._async_client.read_buffer(port, max_lines))

    def subscribe_serial_output(
        self,
        port: str,
        callback: Callable[[dict[str, Any]], None],
    ) -> str:
        """Subscribe to serial output events.

        Args:
            port: Serial port
            callback: Function to call on output

        Returns:
            Subscription ID
        """
        return self._run_async(self._async_client.subscribe_serial_output(port, callback))
