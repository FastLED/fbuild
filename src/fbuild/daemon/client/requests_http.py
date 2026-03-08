"""
HTTP-based Request Handling for Daemon Operations

This module provides HTTP-based request handlers for build, deploy, monitor,
and install dependencies operations, replacing the legacy file-based IPC.

Architecture:
- HTTP POST requests to daemon FastAPI endpoints
- Real-time build output via WebSocket streaming during HTTP long-poll
- Backward-compatible with existing BaseRequestHandler interface
- No file-based communication (request/response files)

Usage:
    >>> from fbuild.daemon.client.requests_http import request_build_http
    >>> success = request_build_http(
    ...     project_dir=Path("/path/to/project"),
    ...     environment="uno",
    ...     clean_build=False,
    ...     verbose=False
    ... )
"""

import asyncio
import json
import logging
import os
import sys
import threading
from pathlib import Path

from fbuild.build.build_profiles import BuildProfile
from fbuild.daemon.client.http_utils import (
    get_daemon_port,
    get_daemon_url,
    serialize_request,
)
from fbuild.daemon.client.interruptible_http import (
    InterruptibleHTTPError,
    interruptible_post,
)
from fbuild.daemon.client.lifecycle import ensure_daemon_running
from fbuild.daemon.messages import (
    BuildRequest,
    DeployRequest,
    InstallDependenciesRequest,
    MonitorRequest,
)

logger = logging.getLogger(__name__)


class _WebSocketStreamer:
    """Stream build output from daemon via WebSocket, printing lines in real-time.

    Connects to the daemon's /ws/status endpoint in a background thread and
    prints any ``build_output`` messages to stdout. Starts before the HTTP POST
    and stops after the HTTP response arrives.
    """

    def __init__(self) -> None:
        self._stop_event = threading.Event()
        self._thread: threading.Thread | None = None

    def start(self) -> None:
        """Start the WebSocket listener in a background thread."""
        try:
            __import__("websockets")
        except ImportError:
            return  # websockets not installed, skip streaming

        self._thread = threading.Thread(
            target=self._run,
            name="WSOutputStreamer",
            daemon=True,
        )
        self._thread.start()

    def stop(self) -> None:
        """Signal the listener to stop and wait for it to finish."""
        self._stop_event.set()
        if self._thread is not None:
            self._thread.join(timeout=2.0)

    def _run(self) -> None:
        """Background thread entry point — runs the async WebSocket listener."""
        try:
            loop = asyncio.new_event_loop()
            asyncio.set_event_loop(loop)
            try:
                loop.run_until_complete(self._listen())
            finally:
                loop.close()
        except KeyboardInterrupt:
            raise
        except Exception:
            pass  # Don't crash the build if streaming fails

    async def _listen(self) -> None:
        """Connect to the daemon WebSocket and print build_output lines."""
        import websockets

        port = get_daemon_port()
        uri = f"ws://127.0.0.1:{port}/ws/status"

        try:
            async with websockets.connect(uri) as ws:
                while not self._stop_event.is_set():
                    try:
                        raw = await asyncio.wait_for(ws.recv(), timeout=0.5)
                        data = json.loads(raw)
                        if data.get("type") == "build_output":
                            line = data.get("line", "")
                            if line:
                                sys.stdout.write(line)
                                sys.stdout.flush()
                    except asyncio.TimeoutError:
                        continue
        except KeyboardInterrupt:
            raise
        except Exception as exc:
            logger.debug(f"WebSocket streamer connection ended: {exc}")


def request_build_http(
    project_dir: Path,
    environment: str,
    clean_build: bool = False,
    verbose: bool = False,
    jobs: int | None = None,
    profile: BuildProfile = BuildProfile.RELEASE,
    generate_compiledb: bool = False,
    timeout: float = 1800,
) -> bool:
    """Submit a build request to the daemon via HTTP.

    Args:
        project_dir: Project directory
        environment: Build environment
        clean_build: Whether to perform clean build
        verbose: Enable verbose build output
        jobs: Number of parallel compilation jobs
        profile: Build profile
        generate_compiledb: Generate compile_commands.json without compiling
        timeout: Request timeout in seconds

    Returns:
        True if build succeeded, False otherwise

    Example:
        >>> from pathlib import Path
        >>> success = request_build_http(
        ...     project_dir=Path("tests/uno"),
        ...     environment="uno",
        ...     clean_build=True,
        ...     verbose=False
        ... )
    """
    # Ensure daemon is running
    try:
        ensure_daemon_running()
    except RuntimeError as e:
        print(f"❌ {e}")
        return False

    # Create build request
    request = BuildRequest(
        project_dir=str(project_dir.absolute()),
        environment=environment,
        clean_build=clean_build,
        verbose=verbose,
        caller_pid=os.getpid(),
        caller_cwd=os.getcwd(),
        jobs=jobs,
        profile=profile,
        generate_compiledb=generate_compiledb,
    )

    # Print submission info
    print("\n📤 Submitting build request...")
    print(f"   Project: {project_dir}")
    print(f"   Environment: {environment}")
    print(f"   Request ID: {request.request_id}")
    if clean_build:
        print("   Clean build: Yes")
    print("   ✅ Submitted\n")

    # Start WebSocket streamer for real-time build output
    streamer = _WebSocketStreamer()
    streamer.start()

    # Submit HTTP request (using interruptible wrapper for proper CTRL-C handling)
    try:
        response = interruptible_post(
            url=get_daemon_url("/api/build"),
            json=serialize_request(request),
            timeout=timeout,
        )

        # Stop streaming — HTTP response means build is done
        streamer.stop()

        if response.status_code == 200:
            result = response.json()
            if result.get("success"):
                return True
            else:
                print(f"❌ Build failed: {result.get('message', 'Unknown error')}")
                return False
        else:
            print(f"❌ HTTP request failed with status {response.status_code}")
            print(f"   {response.text}")
            return False

    except InterruptibleHTTPError as e:
        streamer.stop()
        # Check if it's a timeout or connection error
        error_msg = str(e).lower()
        if "timeout" in error_msg:
            print(f"❌ Build timeout ({timeout}s)")
        elif "connect" in error_msg or "connection" in error_msg:
            print("❌ Failed to connect to daemon")
        else:
            print(f"❌ Build request failed: {e}")
        return False
    except KeyboardInterrupt:
        streamer.stop()
        print("\n⚠️  Build cancelled by user (CTRL-C)")
        raise
    except Exception as e:
        streamer.stop()
        print(f"❌ Build request failed: {e}")
        return False


def request_deploy_http(
    project_dir: Path,
    environment: str,
    port: str | None = None,
    clean_build: bool = False,
    monitor_after: bool = False,
    monitor_timeout: float | None = None,
    monitor_halt_on_error: str | None = None,
    monitor_halt_on_success: str | None = None,
    monitor_expect: str | None = None,
    monitor_show_timestamp: bool = False,
    skip_build: bool = False,
    timeout: float = 1800,
) -> bool:
    """Submit a deploy request to the daemon via HTTP.

    Args:
        project_dir: Project directory
        environment: Build environment
        port: Serial port (optional)
        clean_build: Whether to perform clean build
        monitor_after: Whether to start monitor after deploy
        monitor_timeout: Timeout for monitor
        monitor_halt_on_error: Pattern to halt on error
        monitor_halt_on_success: Pattern to halt on success
        monitor_expect: Expected pattern to check
        monitor_show_timestamp: Whether to prefix output lines with timestamp
        skip_build: Skip build phase
        timeout: Request timeout in seconds

    Returns:
        True if deploy succeeded, False otherwise
    """
    # Ensure daemon is running
    try:
        ensure_daemon_running()
    except RuntimeError as e:
        print(f"❌ {e}")
        return False

    # Create deploy request
    request = DeployRequest(
        project_dir=str(project_dir.absolute()),
        environment=environment,
        port=port,
        clean_build=clean_build,
        monitor_after=monitor_after,
        monitor_timeout=monitor_timeout,
        monitor_halt_on_error=monitor_halt_on_error,
        monitor_halt_on_success=monitor_halt_on_success,
        monitor_expect=monitor_expect,
        monitor_show_timestamp=monitor_show_timestamp,
        skip_build=skip_build,
        caller_pid=os.getpid(),
        caller_cwd=os.getcwd(),
    )

    # Print submission info
    print("\n📤 Submitting deploy request...")
    print(f"   Project: {project_dir}")
    print(f"   Environment: {environment}")
    print(f"   Request ID: {request.request_id}")
    if port:
        print(f"   Port: {port}")
    if skip_build:
        print("   Skip build: Yes")
    print("   ✅ Submitted\n")

    # Start WebSocket streamer for real-time build/deploy output
    streamer = _WebSocketStreamer()
    streamer.start()

    # Submit HTTP request (using interruptible wrapper for proper CTRL-C handling)
    try:
        response = interruptible_post(
            url=get_daemon_url("/api/deploy"),
            json=serialize_request(request),
            timeout=timeout,
        )

        streamer.stop()

        if response.status_code == 200:
            result = response.json()
            if result.get("success"):
                return True
            else:
                print(f"❌ Deploy failed: {result.get('message', 'Unknown error')}")
                return False
        else:
            print(f"❌ HTTP request failed with status {response.status_code}")
            print(f"   {response.text}")
            return False

    except InterruptibleHTTPError as e:
        streamer.stop()
        # Check if it's a timeout or connection error
        error_msg = str(e).lower()
        if "timeout" in error_msg:
            print(f"❌ Deploy timeout ({timeout}s)")
        elif "connect" in error_msg or "connection" in error_msg:
            print("❌ Failed to connect to daemon")
        else:
            print(f"❌ Deploy request failed: {e}")
        return False
    except KeyboardInterrupt:
        streamer.stop()
        print("\n⚠️  Deploy cancelled by user (CTRL-C)")
        raise
    except Exception as e:
        print(f"❌ Deploy request failed: {e}")
        return False


def request_monitor_http(
    project_dir: Path,
    environment: str,
    port: str | None = None,
    baud_rate: int | None = None,
    timeout: float | None = None,
    halt_on_error: str | None = None,
    halt_on_success: str | None = None,
    expect: str | None = None,
    show_timestamp: bool = False,
    request_timeout: float = 1800,
) -> bool:
    """Submit a monitor request to the daemon via HTTP.

    Args:
        project_dir: Project directory
        environment: Build environment
        port: Serial port (optional)
        baud_rate: Serial baud rate (optional, use config default if None)
        timeout: Monitor timeout in seconds
        halt_on_error: Pattern to halt on error
        halt_on_success: Pattern to halt on success
        expect: Expected pattern to check
        show_timestamp: Whether to prefix output lines with timestamp
        request_timeout: HTTP request timeout in seconds

    Returns:
        True if monitor succeeded, False otherwise
    """
    # Ensure daemon is running
    try:
        ensure_daemon_running()
    except RuntimeError as e:
        print(f"❌ {e}")
        return False

    # Create monitor request
    request = MonitorRequest(
        project_dir=str(project_dir.absolute()),
        environment=environment,
        port=port,
        baud_rate=baud_rate,
        timeout=timeout,
        halt_on_error=halt_on_error,
        halt_on_success=halt_on_success,
        expect=expect,
        show_timestamp=show_timestamp,
        caller_pid=os.getpid(),
        caller_cwd=os.getcwd(),
    )

    # Print submission info
    print("\n📤 Submitting monitor request...")
    print(f"   Project: {project_dir}")
    print(f"   Environment: {environment}")
    print(f"   Request ID: {request.request_id}")
    if port:
        print(f"   Port: {port}")
    print("   ✅ Submitted\n")

    # Submit HTTP request (using interruptible wrapper for proper CTRL-C handling)
    try:
        response = interruptible_post(
            url=get_daemon_url("/api/monitor"),
            json=serialize_request(request),
            timeout=request_timeout,
        )

        if response.status_code == 200:
            result = response.json()
            print("👁️  Monitor Progress:")
            print(f"   Status: {result.get('message', 'Success')}")
            if result.get("success"):
                print("✅ Monitor completed")
                return True
            else:
                print(f"❌ Monitor failed: {result.get('message', 'Unknown error')}")
                return False
        else:
            print(f"❌ HTTP request failed with status {response.status_code}")
            print(f"   {response.text}")
            return False

    except InterruptibleHTTPError as e:
        # Check if it's a timeout or connection error
        error_msg = str(e).lower()
        if "timeout" in error_msg:
            print(f"❌ Monitor timeout ({request_timeout}s)")
        elif "connect" in error_msg or "connection" in error_msg:
            print("❌ Failed to connect to daemon")
        else:
            print(f"❌ Monitor request failed: {e}")
        return False
    except KeyboardInterrupt:
        print("\n⚠️  Monitor cancelled by user (CTRL-C)")
        raise
    except Exception as e:
        print(f"❌ Monitor request failed: {e}")
        return False


def request_install_dependencies_http(
    project_dir: Path,
    environment: str,
    verbose: bool = False,
    timeout: float = 1800,
) -> bool:
    """Submit an install dependencies request to the daemon via HTTP.

    Args:
        project_dir: Project directory
        environment: Build environment
        verbose: Enable verbose output
        timeout: Request timeout in seconds

    Returns:
        True if install succeeded, False otherwise
    """
    # Ensure daemon is running
    try:
        ensure_daemon_running()
    except RuntimeError as e:
        print(f"❌ {e}")
        return False

    # Create install dependencies request
    request = InstallDependenciesRequest(
        project_dir=str(project_dir.absolute()),
        environment=environment,
        verbose=verbose,
        caller_pid=os.getpid(),
        caller_cwd=os.getcwd(),
    )

    # Print submission info
    print("\n📤 Submitting install dependencies request...")
    print(f"   Project: {project_dir}")
    print(f"   Environment: {environment}")
    print(f"   Request ID: {request.request_id}")
    print("   ✅ Submitted\n")

    # Submit HTTP request (using interruptible wrapper for proper CTRL-C handling)
    try:
        response = interruptible_post(
            url=get_daemon_url("/api/install-deps"),
            json=serialize_request(request),
            timeout=timeout,
        )

        if response.status_code == 200:
            result = response.json()
            print("📦 Install Dependencies Progress:")
            print(f"   Status: {result.get('message', 'Success')}")
            if result.get("success"):
                print("✅ Install dependencies completed")
                return True
            else:
                print(f"❌ Install dependencies failed: {result.get('message', 'Unknown error')}")
                return False
        else:
            print(f"❌ HTTP request failed with status {response.status_code}")
            print(f"   {response.text}")
            return False

    except InterruptibleHTTPError as e:
        # Check if it's a timeout or connection error
        error_msg = str(e).lower()
        if "timeout" in error_msg:
            print(f"❌ Install dependencies timeout ({timeout}s)")
        elif "connect" in error_msg or "connection" in error_msg:
            print("❌ Failed to connect to daemon")
        else:
            print(f"❌ Install dependencies request failed: {e}")
        return False
    except KeyboardInterrupt:
        print("\n⚠️  Install dependencies cancelled by user (CTRL-C)")
        raise
    except Exception as e:
        print(f"❌ Install dependencies request failed: {e}")
        return False
