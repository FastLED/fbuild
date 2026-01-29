"""
Client-side daemon connection object.

This module provides the DaemonConnection class which represents a client's
connection to the fbuild daemon. Each call to connect_daemon() creates a NEW
connection with a unique ID - connections are NOT singletons.

Usage:
    from fbuild.daemon.connection import connect_daemon

    # Using context manager (recommended)
    with connect_daemon(Path("./project"), "esp32dev") as conn:
        conn.install_dependencies()
        conn.build(clean=True)
        conn.deploy(port="/dev/ttyUSB0")
        conn.monitor()

    # Manual lifecycle management
    conn = connect_daemon(Path("./project"), "esp32dev")
    try:
        conn.build()
    finally:
        conn.close()
"""

import _thread
import json
import os
import socket
import time
import uuid
from pathlib import Path
from threading import Thread
from typing import Any

from fbuild.daemon.messages import (
    ClientConnectRequest,
    ClientDisconnectRequest,
    ClientHeartbeatRequest,
)


def _get_daemon_dir(dev_mode: bool) -> Path:
    """Get daemon directory based on dev mode setting.

    Args:
        dev_mode: Whether to use development mode directory.

    Returns:
        Path to daemon directory.
    """
    if dev_mode:
        return Path.cwd() / ".fbuild" / "daemon_dev"
    else:
        return Path.home() / ".fbuild" / "daemon"


class DaemonConnection:
    """Client-side connection to the fbuild daemon.

    Represents a single client connection with a unique ID. Each connection
    maintains its own heartbeat thread and can perform operations independently.

    IMPORTANT: This is NOT a singleton. Each call to connect_daemon() creates
    a new DaemonConnection instance with a unique connection_id.

    Attributes:
        connection_id: Unique UUID for this connection.
        project_dir: Path to the project directory.
        environment: Build environment name.
        dev_mode: Whether using development mode daemon.
    """

    def __init__(
        self,
        project_dir: Path,
        environment: str,
        dev_mode: bool | None = None,
    ) -> None:
        """Initialize a new daemon connection.

        Args:
            project_dir: Path to project directory.
            environment: Build environment name.
            dev_mode: Use dev mode daemon. Auto-detects from FBUILD_DEV_MODE if None.
        """
        self.connection_id: str = str(uuid.uuid4())
        self.project_dir: Path = Path(project_dir).resolve()
        self.environment: str = environment

        # Auto-detect dev mode from environment variable if not specified
        if dev_mode is None:
            self.dev_mode: bool = os.environ.get("FBUILD_DEV_MODE") == "1"
        else:
            self.dev_mode = dev_mode

        self._closed: bool = False
        self._heartbeat_thread: Thread | None = None
        self._heartbeat_interval: float = 10.0  # seconds between heartbeats

        # Get daemon directory based on dev mode
        self._daemon_dir = _get_daemon_dir(self.dev_mode)

        # Send connect message to daemon
        self._send_connect()

        # Start heartbeat thread
        self._start_heartbeat()

    def __enter__(self) -> "DaemonConnection":
        """Context manager entry.

        Returns:
            This connection instance.
        """
        return self

    def __exit__(self, _exc_type: type[BaseException] | None, _exc_val: BaseException | None, _exc_tb: Any) -> None:
        """Context manager exit - closes the connection."""
        self.close()

    def _start_heartbeat(self) -> None:
        """Start the background heartbeat thread.

        The heartbeat thread sends periodic heartbeats to the daemon to indicate
        this connection is still alive. If heartbeats stop, the daemon will
        eventually clean up this connection's resources.
        """

        def heartbeat_loop() -> None:
            while not self._closed:
                try:
                    self._send_heartbeat()
                except KeyboardInterrupt:
                    _thread.interrupt_main()
                    break  # Exit heartbeat loop on interrupt
                except Exception:
                    # Silently ignore heartbeat failures - daemon may not be ready
                    pass

                # Sleep in small increments to allow faster exit when closed
                sleep_time = 0.0
                while sleep_time < self._heartbeat_interval and not self._closed:
                    time.sleep(0.5)
                    sleep_time += 0.5

        self._heartbeat_thread = Thread(target=heartbeat_loop, daemon=True)
        self._heartbeat_thread.start()

    def _send_heartbeat(self) -> None:
        """Send a heartbeat message to the daemon.

        Writes a heartbeat file to the daemon directory that the daemon
        will pick up during its polling cycle.
        """
        if self._closed:
            return

        request = ClientHeartbeatRequest(
            client_id=self.connection_id,
            timestamp=time.time(),
        )

        heartbeat_file = self._daemon_dir / f"heartbeat_{self.connection_id}.json"
        self._daemon_dir.mkdir(parents=True, exist_ok=True)

        # Atomic write using temp file
        temp_file = heartbeat_file.with_suffix(".tmp")
        try:
            with open(temp_file, "w") as f:
                json.dump(request.to_dict(), f)
            temp_file.replace(heartbeat_file)
        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception:
            # Best effort - don't fail if we can't write heartbeat
            if temp_file.exists():
                temp_file.unlink(missing_ok=True)

    def _send_connect(self) -> None:
        """Send a connect message to register with the daemon.

        Called during __init__ to register this connection with the daemon.
        """
        request = ClientConnectRequest(
            client_id=self.connection_id,
            pid=os.getpid(),
            hostname=socket.gethostname(),
            version=self._get_version(),
            timestamp=time.time(),
        )

        connect_file = self._daemon_dir / f"connect_{self.connection_id}.json"
        self._daemon_dir.mkdir(parents=True, exist_ok=True)

        # Atomic write
        temp_file = connect_file.with_suffix(".tmp")
        try:
            with open(temp_file, "w") as f:
                json.dump(request.to_dict(), f)
            temp_file.replace(connect_file)
        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception:
            # Best effort - daemon may not be running yet
            if temp_file.exists():
                temp_file.unlink(missing_ok=True)

    def _send_disconnect(self) -> None:
        """Send a disconnect message to notify daemon of graceful close.

        Called during close() to notify the daemon to clean up resources
        associated with this connection.
        """
        request = ClientDisconnectRequest(
            client_id=self.connection_id,
            reason="graceful_close",
            timestamp=time.time(),
        )

        disconnect_file = self._daemon_dir / f"disconnect_{self.connection_id}.json"
        self._daemon_dir.mkdir(parents=True, exist_ok=True)

        # Atomic write
        temp_file = disconnect_file.with_suffix(".tmp")
        try:
            with open(temp_file, "w") as f:
                json.dump(request.to_dict(), f)
            temp_file.replace(disconnect_file)
        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception:
            # Best effort
            if temp_file.exists():
                temp_file.unlink(missing_ok=True)

        # Clean up heartbeat file if it exists
        heartbeat_file = self._daemon_dir / f"heartbeat_{self.connection_id}.json"
        heartbeat_file.unlink(missing_ok=True)

        # Clean up connect file if it exists
        connect_file = self._daemon_dir / f"connect_{self.connection_id}.json"
        connect_file.unlink(missing_ok=True)

    def _get_version(self) -> str:
        """Get the fbuild version string.

        Returns:
            Version string from fbuild package.
        """
        try:
            from fbuild import __version__

            return __version__
        except ImportError:
            return "unknown"

    def _check_closed(self) -> None:
        """Raise RuntimeError if connection is closed.

        Raises:
            RuntimeError: If the connection has been closed.
        """
        if self._closed:
            raise RuntimeError(f"DaemonConnection {self.connection_id} is closed. Cannot perform operations on a closed connection.")

    def close(self) -> None:
        """Gracefully close the connection.

        Stops the heartbeat thread, sends a disconnect message to the daemon,
        and marks the connection as closed. Safe to call multiple times.
        """
        if self._closed:
            return

        self._closed = True

        # Send disconnect message to daemon
        self._send_disconnect()

        # Wait for heartbeat thread to stop (with timeout)
        if self._heartbeat_thread is not None:
            self._heartbeat_thread.join(timeout=1.0)
            self._heartbeat_thread = None

    # =========================================================================
    # Operation Methods
    # =========================================================================

    def install_dependencies(
        self,
        verbose: bool = False,
        timeout: float = 1800,
    ) -> bool:
        """Install project dependencies (toolchain, framework, libraries).

        This pre-downloads and caches all dependencies required for a build
        without actually compiling.

        Args:
            verbose: Enable verbose output.
            timeout: Maximum wait time in seconds (default: 30 minutes).

        Returns:
            True if dependencies installed successfully, False otherwise.

        Raises:
            RuntimeError: If the connection is closed.
        """
        self._check_closed()

        from fbuild.daemon.client import request_install_dependencies_http

        return request_install_dependencies_http(
            project_dir=self.project_dir,
            environment=self.environment,
            verbose=verbose,
            timeout=timeout,
        )

    def build(
        self,
        clean: bool = False,
        verbose: bool = False,
        timeout: float = 1800,
    ) -> bool:
        """Build the project.

        Args:
            clean: Whether to perform a clean build.
            verbose: Enable verbose build output.
            timeout: Maximum wait time in seconds (default: 30 minutes).

        Returns:
            True if build successful, False otherwise.

        Raises:
            RuntimeError: If the connection is closed.
        """
        self._check_closed()

        from fbuild.daemon.client import request_build_http

        return request_build_http(
            project_dir=self.project_dir,
            environment=self.environment,
            clean_build=clean,
            verbose=verbose,
            timeout=timeout,
        )

    def deploy(
        self,
        port: str | None = None,
        clean: bool = False,
        monitor_after: bool = False,
        monitor_timeout: float | None = None,
        monitor_halt_on_error: str | None = None,
        monitor_halt_on_success: str | None = None,
        monitor_expect: str | None = None,
        monitor_show_timestamp: bool = False,
        skip_build: bool = False,
        timeout: float = 1800,
    ) -> bool:
        """Deploy (build and upload) the project to a device.

        Args:
            port: Serial port for upload (auto-detect if None).
            clean: Whether to perform a clean build.
            monitor_after: Whether to start serial monitor after deploy.
            monitor_timeout: Timeout for monitor in seconds.
            monitor_halt_on_error: Pattern to halt on error.
            monitor_halt_on_success: Pattern to halt on success.
            monitor_expect: Expected pattern to check.
            monitor_show_timestamp: Prefix output with elapsed time.
            skip_build: Skip build phase and upload existing firmware (upload-only mode).
            timeout: Maximum wait time in seconds (default: 30 minutes).

        Returns:
            True if deploy successful, False otherwise.

        Raises:
            RuntimeError: If the connection is closed.
        """
        self._check_closed()

        from fbuild.daemon.client import request_deploy_http

        return request_deploy_http(
            project_dir=self.project_dir,
            environment=self.environment,
            port=port,
            clean_build=clean,
            monitor_after=monitor_after,
            monitor_timeout=monitor_timeout,
            monitor_halt_on_error=monitor_halt_on_error,
            monitor_halt_on_success=monitor_halt_on_success,
            monitor_expect=monitor_expect,
            timeout=timeout,
        )

    def monitor(
        self,
        port: str | None = None,
        baud_rate: int | None = None,
        halt_on_error: str | None = None,
        halt_on_success: str | None = None,
        expect: str | None = None,
        timeout: float | None = None,
        show_timestamp: bool = False,
    ) -> bool:
        """Start serial monitoring.

        Args:
            port: Serial port (auto-detect if None).
            baud_rate: Serial baud rate (use config default if None).
            halt_on_error: Pattern to halt on error.
            halt_on_success: Pattern to halt on success.
            expect: Expected pattern to check at timeout/success.
            timeout: Maximum monitoring time in seconds.
            show_timestamp: Prefix output lines with elapsed time.

        Returns:
            True if monitoring completed successfully, False otherwise.

        Raises:
            RuntimeError: If the connection is closed.
        """
        self._check_closed()

        from fbuild.daemon.client import request_monitor_http

        return request_monitor_http(
            project_dir=self.project_dir,
            environment=self.environment,
            port=port,
            baud_rate=baud_rate,
            halt_on_error=halt_on_error,
            halt_on_success=halt_on_success,
            expect=expect,
            timeout=timeout,
        )

    def get_status(self) -> dict[str, Any]:
        """Get current daemon status.

        Returns:
            Dictionary with daemon status information including:
            - running: Whether daemon is running
            - state: Current daemon state
            - message: Status message
            - pid: Daemon process ID
            - locks: Lock information

        Raises:
            RuntimeError: If the connection is closed.
        """
        self._check_closed()

        from fbuild.daemon.client import get_daemon_status

        return get_daemon_status()


def connect_daemon(
    project_dir: Path | str,
    environment: str,
    dev_mode: bool | None = None,
) -> DaemonConnection:
    """Create a new daemon connection.

    Each call creates a NEW connection with a unique ID. This is NOT a
    singleton - multiple connections can exist simultaneously for different
    projects or environments.

    Usage:
        # Using context manager (recommended)
        with connect_daemon(Path("./project"), "esp32dev") as conn:
            conn.build()
            conn.deploy()

        # Manual lifecycle
        conn = connect_daemon(Path("./project"), "esp32dev")
        try:
            conn.build()
        finally:
            conn.close()

    Args:
        project_dir: Path to project directory.
        environment: Build environment name.
        dev_mode: Use dev mode daemon. Auto-detects from FBUILD_DEV_MODE if None.

    Returns:
        New DaemonConnection instance.
    """
    return DaemonConnection(
        project_dir=Path(project_dir),
        environment=environment,
        dev_mode=dev_mode,
    )
