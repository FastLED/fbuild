"""
fbuild Daemon - Concurrent Deploy and Monitor Management

This daemon manages deploy and monitor operations to prevent resource conflicts
when multiple operations are running. The daemon:

1. Runs as a singleton process (enforced via PID file)
2. Survives client termination
3. Processes requests with appropriate locking (per-port, per-project)
4. Provides status updates via status file
5. Auto-shuts down after idle timeout
6. Cleans up orphaned processes

Architecture:
    Clients -> Request File -> Daemon -> Deploy/Monitor Process
                   |              |
                   v              v
              Status File    Progress Updates
"""

import _thread
import atexit
import json
import logging
import multiprocessing
import os
import signal
import sys
import threading
import time
from dataclasses import dataclass, field
from logging.handlers import TimedRotatingFileHandler
from pathlib import Path
from typing import Any, Callable

from fbuild.daemon.connection_registry import ConnectionRegistry
from fbuild.daemon.daemon_context import (
    DaemonContext,
    cleanup_daemon_context,
    create_daemon_context,
)
from fbuild.daemon.messages import (
    DaemonState,
    SerialMonitorAttachRequest,
    SerialMonitorDetachRequest,
    SerialMonitorPollRequest,
    SerialWriteRequest,
)
from fbuild.daemon.paths import (
    DAEMON_DIR,
    FILE_CACHE_FILE,
    LOG_FILE,
    PID_FILE,
    PROCESS_REGISTRY_FILE,
    STATUS_FILE,
)
from fbuild.daemon.process_tracker import ProcessTracker
from fbuild.daemon.processors.serial_monitor_processor import SerialMonitorAPIProcessor

# Type variable removed - no longer needed for file-based operation requests

# Module-level daemon context accessor for cross-module access
_daemon_context: DaemonContext | None = None

# Additional device files not in paths module yet
DEVICE_PREEMPT_REQUEST_FILE = DAEMON_DIR / "device_preempt_request.json"
DEVICE_PREEMPT_RESPONSE_FILE = DAEMON_DIR / "device_preempt_response.json"

# Serial Monitor API request/response files (used by fbuild.api.SerialMonitor)
SERIAL_MONITOR_ATTACH_REQUEST_FILE = DAEMON_DIR / "serial_monitor_attach_request.json"
SERIAL_MONITOR_DETACH_REQUEST_FILE = DAEMON_DIR / "serial_monitor_detach_request.json"
SERIAL_MONITOR_POLL_REQUEST_FILE = DAEMON_DIR / "serial_monitor_poll_request.json"
SERIAL_MONITOR_RESPONSE_FILE = DAEMON_DIR / "serial_monitor_response.json"
SERIAL_WRITE_REQUEST_FILE = DAEMON_DIR / "serial_write_request.json"
SERIAL_WRITE_RESPONSE_FILE = DAEMON_DIR / "serial_write_response.json"

# Connection management file patterns
CONNECTION_FILES_PATTERN = "connect_*.json"
HEARTBEAT_FILES_PATTERN = "heartbeat_*.json"
DISCONNECT_FILES_PATTERN = "disconnect_*.json"

ORPHAN_CHECK_INTERVAL = 5  # Check for orphaned processes every 5 seconds
STALE_LOCK_CHECK_INTERVAL = 60  # Check for stale locks every 60 seconds
DEAD_CLIENT_CHECK_INTERVAL = 10  # Check for dead clients every 10 seconds
IDLE_TIMEOUT = 43200  # 12 hours (fallback)
# Self-eviction timeout: if daemon has 0 clients AND 0 ops for this duration, shutdown
# Increased to 120s to accommodate validation workflows with compilation (deploy + compile + upload + port check)
SELF_EVICTION_TIMEOUT = 120.0  # 120 seconds - accommodates validation workflows with compilation


def set_daemon_context(context: DaemonContext) -> None:
    """Set the daemon context (called by run_daemon_loop).

    This function is called internally by run_daemon_loop() to make the daemon
    context accessible to other modules via get_compilation_queue().

    Args:
        context: The daemon context to set

    Example:
        >>> context = create_daemon_context(...)
        >>> set_daemon_context(context)
        >>> # Now other modules can call get_compilation_queue()
    """
    global _daemon_context
    _daemon_context = context


# NOTE: RequestConfig dataclass removed - no longer needed for file-based operation requests


@dataclass
class DeviceRequestConfig:
    """Configuration for a device management request."""

    request_file: Path
    response_file: Path
    handler: Callable[[dict[str, Any], DaemonContext], dict[str, Any]]
    lock: threading.Lock = field(default_factory=threading.Lock)


@dataclass
class PeriodicTask:
    """Configuration for a periodic daemon task."""

    name: str
    interval: float
    callback: Callable[[], None]
    last_run: float = 0.0

    def should_run(self) -> bool:
        """Check if enough time has passed since last run."""
        return time.time() - self.last_run >= self.interval

    def run(self) -> None:
        """Execute the task and update last run time."""
        try:
            self.callback()
            self.last_run = time.time()
        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception as e:
            logging.error(f"Error in periodic task '{self.name}': {e}", exc_info=True)


def setup_logging(foreground: bool = False) -> None:
    """Setup logging for daemon."""
    DAEMON_DIR.mkdir(parents=True, exist_ok=True)

    # Enhanced log format with function name and line number
    LOG_FORMAT = "%(asctime)s - %(name)s - %(levelname)s - [%(funcName)s:%(lineno)d] - %(message)s"
    LOG_DATEFMT = "%Y-%m-%d %H:%M:%S"

    # Configure root logger
    logger = logging.getLogger()
    logger.setLevel(logging.DEBUG)  # CHANGED: Enable DEBUG logging

    # Console handler (for foreground mode)
    if foreground:
        console_handler = logging.StreamHandler(sys.stdout)
        console_handler.setLevel(logging.DEBUG)  # CHANGED: Enable DEBUG logging
        console_formatter = logging.Formatter(fmt=LOG_FORMAT, datefmt=LOG_DATEFMT)
        console_handler.setFormatter(console_formatter)
        logger.addHandler(console_handler)

    # Timed rotating file handler (always) - rotates daily at midnight
    file_handler = TimedRotatingFileHandler(
        str(LOG_FILE),
        when="midnight",  # Rotate at midnight
        interval=1,  # Daily rotation
        backupCount=2,  # Keep 2 days of backups (total 3 files)
        utc=False,  # Use local time
        atTime=None,  # Rotate exactly at midnight
    )
    file_handler.setLevel(logging.DEBUG)  # CHANGED: Enable DEBUG logging
    file_formatter = logging.Formatter(fmt=LOG_FORMAT, datefmt=LOG_DATEFMT)
    file_handler.setFormatter(file_formatter)
    logger.addHandler(file_handler)


# NOTE: read_request_file() and clear_request_file() removed
# Operations now use FastAPI HTTP endpoints instead of file-based requests


def should_shutdown() -> bool:
    """Check if daemon should shutdown.

    Returns:
        True if shutdown signal detected, False otherwise
    """
    # Check for shutdown signal file
    shutdown_file = DAEMON_DIR / "shutdown.signal"
    if shutdown_file.exists():
        logging.info("Shutdown signal detected")
        try:
            shutdown_file.unlink()
        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception as e:
            logging.warning(f"Failed to remove shutdown signal file: {e}")
        return True
    return False


def cleanup_stale_cancel_signals() -> None:
    """Clean up stale cancel signal files (older than 5 minutes)."""
    try:
        signal_files = list(DAEMON_DIR.glob("cancel_*.signal"))
        logging.debug(f"Found {len(signal_files)} cancel signal files")

        cleaned_count = 0
        for signal_file in signal_files:
            try:
                # Check file age
                file_age = time.time() - signal_file.stat().st_mtime
                if file_age > 300:  # 5 minutes
                    logging.info(f"Cleaning up stale cancel signal: {signal_file.name} (age: {file_age:.1f}s)")
                    signal_file.unlink()
                    cleaned_count += 1
            except KeyboardInterrupt:
                _thread.interrupt_main()
                raise
            except Exception as e:
                logging.warning(f"Failed to clean up {signal_file.name}: {e}")

        if cleaned_count > 0:
            logging.info(f"Cleaned up {cleaned_count} cancel signal files")
    except KeyboardInterrupt:
        _thread.interrupt_main()
        raise
    except Exception as e:
        logging.error(f"Error during cancel signal cleanup: {e}")


def signal_handler(signum: int, frame: object, context: DaemonContext) -> None:
    """Handle SIGTERM/SIGINT - refuse shutdown during operation."""
    signal_name = "SIGTERM" if signum == signal.SIGTERM else "SIGINT"
    logging.info(f"Signal handler invoked: received {signal_name} (signal number {signum})")

    if context.status_manager.get_operation_in_progress():
        logging.warning(f"Received {signal_name} during active operation. Refusing graceful shutdown.")
        print(
            f"\n⚠️  {signal_name} received during operation\n⚠️  Cannot shutdown gracefully while operation is active\n⚠️  Use 'kill -9 {os.getpid()}' to force termination\n",
            flush=True,
        )
        return  # Refuse shutdown
    else:
        logging.info(f"Received {signal_name}, shutting down gracefully (no operation in progress)")
        cleanup_and_exit(context)


def cleanup_and_exit(context: DaemonContext) -> None:
    """Clean up daemon state and exit."""
    logging.info("Daemon shutting down")

    # Shutdown subsystems
    cleanup_daemon_context(context)

    # Remove PID file
    try:
        PID_FILE.unlink(missing_ok=True)
    except KeyboardInterrupt:
        _thread.interrupt_main()
        raise
    except Exception as e:
        logging.error(f"Failed to remove PID file: {e}")

    # Set final status - but preserve COMPLETED/FAILED states so clients can see them
    current_status = context.status_manager.read_status()
    if current_status.state not in (DaemonState.COMPLETED, DaemonState.FAILED):
        context.status_manager.update_status(DaemonState.IDLE, "Daemon shut down")
    else:
        # Preserve completion status but add shutdown note
        logging.info(f"Preserving final status {current_status.state.value} during shutdown")

    logging.info("Cleanup complete, exiting with status 0")
    sys.exit(0)


# NOTE: File-based device/lock/operation request handlers removed
# All device management, lock management, and operations (build/deploy/monitor) now via FastAPI HTTP
# Serial Monitor API still uses file-based IPC (see handle_serial_monitor_* functions below)


# ============================================================================
# Serial Monitor API File-Based IPC (still in use by fbuild.api.SerialMonitor)
# ============================================================================
# NOTE: These handlers are KEPT because fbuild.api.SerialMonitor still uses file-based IPC
# TODO: Migrate SerialMonitor API to WebSockets in a future iteration

# Global processor instance (reused across requests)
_serial_monitor_processor = SerialMonitorAPIProcessor()


def handle_serial_monitor_request(config: DeviceRequestConfig, context: DaemonContext) -> bool:
    """Handle a serial monitor API request file if it exists.

    This is a simplified version of the old handle_device_request that only handles
    serial monitor API requests (attach, detach, poll, write).

    Args:
        config: Device request configuration
        context: Daemon context

    Returns:
        True if a request was processed, False otherwise
    """
    if not config.request_file.exists():
        return False

    try:
        with open(config.request_file) as f:
            request_data = json.load(f)

        # Clear request file immediately (atomic consumption)
        config.request_file.unlink(missing_ok=True)

        # Process request
        response_data = config.handler(request_data, context)

        # Serial monitor API uses per-client response files
        client_id = response_data.pop("_client_id", None)
        if client_id:
            response_file = config.response_file.parent / f"{config.response_file.stem}_{client_id}.json"
        else:
            response_file = config.response_file

        # Write response atomically
        temp_file = response_file.with_suffix(".tmp")
        with open(temp_file, "w") as f:
            json.dump(response_data, f, indent=2)
        temp_file.replace(response_file)

        return True

    except json.JSONDecodeError as e:
        logging.error(f"Invalid JSON in request file {config.request_file}: {e}")
        config.request_file.unlink(missing_ok=True)
        return False
    except KeyboardInterrupt:
        _thread.interrupt_main()
        raise
    except Exception as e:
        logging.error(f"Error handling serial monitor request {config.request_file}: {e}")
        try:
            with open(config.response_file, "w") as f:
                json.dump({"success": False, "message": str(e)}, f)
        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception:
            pass
        return False


def handle_serial_monitor_attach_request(request_data: dict[str, Any], context: DaemonContext) -> dict[str, Any]:
    """Handle Serial Monitor API attach request.

    Args:
        request_data: Raw request dictionary
        context: Daemon context

    Returns:
        Response dictionary (includes client_id for response file routing)
    """
    try:
        request = SerialMonitorAttachRequest.from_dict(request_data)
        response = _serial_monitor_processor.handle_attach(request, context)
        response_dict = response.to_dict()
        # Include client_id in response for per-client file routing
        response_dict["_client_id"] = request.client_id
        return response_dict
    except KeyboardInterrupt:
        raise
    except Exception as e:
        logging.error(f"Error handling serial monitor attach: {e}", exc_info=True)
        from fbuild.daemon.messages import SerialMonitorResponse

        response_dict = SerialMonitorResponse(success=False, message=f"Error: {e}").to_dict()
        # Try to include client_id if available
        if "client_id" in request_data:
            response_dict["_client_id"] = request_data["client_id"]
        return response_dict


def handle_serial_monitor_detach_request(request_data: dict[str, Any], context: DaemonContext) -> dict[str, Any]:
    """Handle Serial Monitor API detach request.

    Args:
        request_data: Raw request dictionary
        context: Daemon context

    Returns:
        Response dictionary (includes client_id for response file routing)
    """
    try:
        request = SerialMonitorDetachRequest.from_dict(request_data)
        response = _serial_monitor_processor.handle_detach(request, context)
        response_dict = response.to_dict()
        response_dict["_client_id"] = request.client_id
        return response_dict
    except KeyboardInterrupt:
        raise
    except Exception as e:
        logging.error(f"Error handling serial monitor detach: {e}", exc_info=True)
        from fbuild.daemon.messages import SerialMonitorResponse

        response_dict = SerialMonitorResponse(success=False, message=f"Error: {e}").to_dict()
        if "client_id" in request_data:
            response_dict["_client_id"] = request_data["client_id"]
        return response_dict


def handle_serial_monitor_poll_request(request_data: dict[str, Any], context: DaemonContext) -> dict[str, Any]:
    """Handle Serial Monitor API poll request.

    Args:
        request_data: Raw request dictionary
        context: Daemon context

    Returns:
        Response dictionary (includes client_id for response file routing)
    """
    try:
        request = SerialMonitorPollRequest.from_dict(request_data)
        response = _serial_monitor_processor.handle_poll(request, context)
        response_dict = response.to_dict()
        response_dict["_client_id"] = request.client_id
        return response_dict
    except KeyboardInterrupt:
        raise
    except Exception as e:
        logging.debug(f"Error handling serial monitor poll: {e}")  # Debug level to avoid spam
        from fbuild.daemon.messages import SerialMonitorResponse

        response_dict = SerialMonitorResponse(success=False, message=f"Error: {e}").to_dict()
        if "client_id" in request_data:
            response_dict["_client_id"] = request_data["client_id"]
        return response_dict


def handle_serial_write_request(request_data: dict[str, Any], context: DaemonContext) -> dict[str, Any]:
    """Handle Serial Write request (used by both CLI and API).

    Args:
        request_data: Raw request dictionary
        context: Daemon context

    Returns:
        Response dictionary (includes client_id for response file routing)
    """
    try:
        request = SerialWriteRequest.from_dict(request_data)
        response = _serial_monitor_processor.handle_write(request, context)
        response_dict = response.to_dict()
        response_dict["_client_id"] = request.client_id
        return response_dict
    except KeyboardInterrupt:
        raise
    except Exception as e:
        logging.error(f"Error handling serial write: {e}", exc_info=True)
        from fbuild.daemon.messages import SerialMonitorResponse

        response_dict = SerialMonitorResponse(success=False, message=f"Error: {e}").to_dict()
        if "client_id" in request_data:
            response_dict["_client_id"] = request_data["client_id"]
        return response_dict


def process_connection_files(registry: ConnectionRegistry, daemon_dir: Path) -> None:
    """Process connection/heartbeat/disconnect files from clients."""
    # Process connect files
    for connect_file in daemon_dir.glob("connect_*.json"):
        try:
            with open(connect_file) as f:
                data = json.load(f)

            # Extract connection ID from filename
            conn_id = connect_file.stem.replace("connect_", "")

            # Register the connection
            registry.register_connection(
                connection_id=data.get("client_id", conn_id),
                project_dir=data.get("project_dir", ""),
                environment=data.get("environment", ""),
                platform=data.get("platform", ""),
                client_pid=data.get("pid", 0),
                client_hostname=data.get("hostname", ""),
                client_version=data.get("version", ""),
            )

            # Remove processed file
            connect_file.unlink(missing_ok=True)
            logging.info(f"Registered connection from {data.get('hostname')} pid={data.get('pid')}")
        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception as e:
            logging.error(f"Error processing connect file {connect_file}: {e}")
            connect_file.unlink(missing_ok=True)

    # Process heartbeat files
    for heartbeat_file in daemon_dir.glob("heartbeat_*.json"):
        try:
            with open(heartbeat_file) as f:
                data = json.load(f)

            conn_id = data.get("client_id", heartbeat_file.stem.replace("heartbeat_", ""))
            registry.update_heartbeat(conn_id)

            # Remove processed file
            heartbeat_file.unlink(missing_ok=True)
        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception as e:
            logging.debug(f"Error processing heartbeat file {heartbeat_file}: {e}")
            heartbeat_file.unlink(missing_ok=True)

    # Process disconnect files
    for disconnect_file in daemon_dir.glob("disconnect_*.json"):
        try:
            with open(disconnect_file) as f:
                data = json.load(f)

            conn_id = data.get("client_id", disconnect_file.stem.replace("disconnect_", ""))
            registry.unregister_connection(conn_id)

            # Remove processed file
            disconnect_file.unlink(missing_ok=True)
            logging.info(f"Unregistered connection {conn_id} (reason: {data.get('reason', 'unknown')})")
        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception as e:
            logging.error(f"Error processing disconnect file {disconnect_file}: {e}")
            disconnect_file.unlink(missing_ok=True)


def start_fastapi_server(context: DaemonContext) -> threading.Thread | None:
    """Start FastAPI HTTP server in a background thread.

    Args:
        context: Daemon context to pass to FastAPI app

    Returns:
        Thread object if successful, None otherwise
    """
    try:
        import uvicorn

        from fbuild.daemon.client.http_utils import write_port_file
        from fbuild.daemon.fastapi_app import (
            create_app,
            get_daemon_port,
            set_daemon_context,
        )

        # Set daemon context for FastAPI dependency injection
        set_daemon_context(context)

        # Get port based on dev mode
        port = get_daemon_port()

        # Write port file for client discovery
        write_port_file(port)

        # Create FastAPI app
        app = create_app()

        # Configure uvicorn
        config = uvicorn.Config(
            app,
            host="127.0.0.1",
            port=port,
            log_level="info",
            access_log=False,  # Disable access logs to reduce noise
        )
        server = uvicorn.Server(config)

        # Run server in background thread
        def run_server():
            try:
                server.run()
            except KeyboardInterrupt:
                raise
            except Exception as e:
                logging.error(f"FastAPI server error: {e}", exc_info=True)

        thread = threading.Thread(target=run_server, daemon=True, name="FastAPI-Server")
        thread.start()

        logging.info(f"FastAPI HTTP server started on http://127.0.0.1:{port}")
        return thread

    except ImportError as e:
        logging.error(f"Failed to import FastAPI dependencies: {e}")
        return None
    except KeyboardInterrupt:
        raise
    except Exception as e:
        logging.error(f"Failed to start FastAPI server: {e}", exc_info=True)
        return None


def run_daemon_loop() -> None:
    """Main daemon loop: process build, deploy and monitor requests."""
    daemon_pid = os.getpid()
    daemon_started_at = time.time()

    logging.info("Starting daemon loop...")

    # Determine optimal worker pool size
    try:
        num_workers = multiprocessing.cpu_count()
    except (ImportError, NotImplementedError) as e:
        num_workers = 4  # Fallback for systems without multiprocessing
        logging.warning(f"Could not detect CPU count ({e}), using fallback: {num_workers} workers")

    # Create daemon context (includes status manager)
    context = create_daemon_context(
        daemon_pid=daemon_pid,
        daemon_started_at=daemon_started_at,
        num_workers=num_workers,
        file_cache_path=FILE_CACHE_FILE,
        status_file_path=STATUS_FILE,
        daemon_dir=DAEMON_DIR,
    )

    # Set module-level context for cross-module access (enables get_compilation_queue())
    set_daemon_context(context)

    # Create connection registry for file-based client connection tracking
    connection_registry = ConnectionRegistry(heartbeat_timeout=30.0)

    # Write initial IDLE status IMMEDIATELY to prevent clients from reading stale status
    context.status_manager.update_status(DaemonState.IDLE, "Daemon starting...")

    # Start async server in background thread for real-time client communication
    if context.async_server is not None:
        logging.info("Starting async server in background thread...")
        context.async_server.start_in_background()
        logging.info("Async server started successfully")
    else:
        logging.warning("Async server not available, clients will use file-based IPC only")

    # Start FastAPI HTTP server in background thread
    logging.info("Starting FastAPI HTTP server...")
    fastapi_thread = start_fastapi_server(context)
    if fastapi_thread:
        logging.info("FastAPI HTTP server started successfully")
    else:
        logging.warning("Failed to start FastAPI HTTP server")

    # Initialize process tracker
    process_tracker = ProcessTracker(PROCESS_REGISTRY_FILE)

    # Register signal handlers
    def signal_handler_wrapper(signum: int, frame: object) -> None:
        signal_handler(signum, frame, context)

    signal.signal(signal.SIGTERM, signal_handler_wrapper)
    signal.signal(signal.SIGINT, signal_handler_wrapper)

    # NOTE: Operation requests (build, deploy, monitor) are now handled via FastAPI HTTP endpoints
    # File-based request processing has been removed in favor of HTTP/WebSocket communication

    # Serial Monitor API still uses file-based IPC (used by fbuild.api.SerialMonitor)
    serial_monitor_requests = [
        DeviceRequestConfig(SERIAL_MONITOR_ATTACH_REQUEST_FILE, SERIAL_MONITOR_RESPONSE_FILE, handle_serial_monitor_attach_request),
        DeviceRequestConfig(SERIAL_MONITOR_DETACH_REQUEST_FILE, SERIAL_MONITOR_RESPONSE_FILE, handle_serial_monitor_detach_request),
        DeviceRequestConfig(SERIAL_MONITOR_POLL_REQUEST_FILE, SERIAL_MONITOR_RESPONSE_FILE, handle_serial_monitor_poll_request),
        DeviceRequestConfig(SERIAL_WRITE_REQUEST_FILE, SERIAL_WRITE_RESPONSE_FILE, handle_serial_write_request),
    ]

    logging.info(f"Daemon started with PID {daemon_pid}")
    context.status_manager.update_status(DaemonState.IDLE, "Daemon ready")

    last_activity = time.time()
    daemon_empty_since: float | None = None

    # Define periodic task callbacks
    def cleanup_orphans() -> None:
        orphaned_clients = process_tracker.cleanup_orphaned_processes()
        if orphaned_clients:
            logging.info(f"Cleaned up orphaned processes for {len(orphaned_clients)} dead clients: {orphaned_clients}")

    def cleanup_cancel_signals() -> None:
        cleanup_stale_cancel_signals()

    def cleanup_dead_clients() -> None:
        dead_clients = context.client_manager.cleanup_dead_clients()
        if dead_clients:
            logging.info(f"Cleaned up {len(dead_clients)} dead clients: {dead_clients}")

    def cleanup_stale_locks() -> None:
        stale_locks = context.lock_manager.get_stale_locks()
        stale_count = len(stale_locks.stale_port_locks) + len(stale_locks.stale_project_locks)
        if stale_count > 0:
            logging.warning(f"Found {stale_count} stale locks, force-releasing...")
            released = context.lock_manager.force_release_stale_locks()
            logging.info(f"Force-released {released} stale locks")
        context.lock_manager.cleanup_unused_locks()

    def process_connections() -> None:
        process_connection_files(connection_registry, DAEMON_DIR)
        cleaned = connection_registry.cleanup_stale_connections()
        if cleaned > 0:
            logging.info(f"Cleaned up {cleaned} stale connections")

    # Configure periodic tasks
    periodic_tasks = [
        PeriodicTask("orphan_cleanup", ORPHAN_CHECK_INTERVAL, cleanup_orphans),
        PeriodicTask("cancel_signal_cleanup", 60, cleanup_cancel_signals),
        PeriodicTask("dead_client_cleanup", DEAD_CLIENT_CHECK_INTERVAL, cleanup_dead_clients),
        PeriodicTask("stale_lock_cleanup", STALE_LOCK_CHECK_INTERVAL, cleanup_stale_locks),
        PeriodicTask("connection_processing", 2, process_connections),
    ]

    logging.info("Entering main daemon loop...")
    iteration_count = 0

    while True:
        try:
            iteration_count += 1
            if iteration_count % 100 == 0:  # Log every 100 iterations to avoid spam
                logging.debug(f"Daemon main loop iteration {iteration_count}")

            # Check for shutdown signal
            if should_shutdown():
                logging.info("Shutdown requested via signal")
                cleanup_and_exit(context)

            # Check idle timeout
            idle_time = time.time() - last_activity
            if idle_time > IDLE_TIMEOUT:
                logging.info(f"Idle timeout reached ({idle_time:.1f}s / {IDLE_TIMEOUT}s), shutting down")
                cleanup_and_exit(context)

            # Self-eviction check: if daemon has 0 clients AND 0 ops for SELF_EVICTION_TIMEOUT, shutdown
            client_count = len(connection_registry.connections)
            operation_running = context.status_manager.get_operation_in_progress()
            daemon_is_empty = client_count == 0 and not operation_running

            if daemon_is_empty:
                if daemon_empty_since is None:
                    daemon_empty_since = time.time()
                    logging.debug("Daemon is now empty (0 clients, 0 ops), starting eviction timer")
                elif time.time() - daemon_empty_since >= SELF_EVICTION_TIMEOUT:
                    logging.info(f"Self-eviction triggered: daemon empty for {time.time() - daemon_empty_since:.1f}s, shutting down")
                    cleanup_and_exit(context)
            elif daemon_empty_since is not None:
                logging.debug(f"Daemon is no longer empty (clients={client_count}, op_running={operation_running})")
                daemon_empty_since = None

            # Run periodic tasks
            for task in periodic_tasks:
                if task.should_run():
                    task.run()

            # Check for manual stale lock clear signal
            clear_locks_signal = DAEMON_DIR / "clear_stale_locks.signal"
            if clear_locks_signal.exists():
                try:
                    clear_locks_signal.unlink()
                    logging.info("Received manual clear stale locks signal")
                    stale_locks = context.lock_manager.get_stale_locks()
                    stale_count = len(stale_locks.stale_port_locks) + len(stale_locks.stale_project_locks)
                    if stale_count > 0:
                        logging.warning(f"Manually clearing {stale_count} stale locks...")
                        released = context.lock_manager.force_release_stale_locks()
                        logging.info(f"Force-released {released} stale locks")
                    else:
                        logging.info("No stale locks to clear")
                except KeyboardInterrupt:
                    _thread.interrupt_main()
                    raise
                except Exception as e:
                    logging.error(f"Error handling clear locks signal: {e}", exc_info=True)

            # NOTE: File-based request processing removed - all operations now via FastAPI HTTP
            # Operations (build, deploy, monitor) are handled by FastAPI endpoints
            # Device/lock management is handled by FastAPI endpoints

            # Process serial monitor API requests (still file-based for fbuild.api.SerialMonitor)
            for config in serial_monitor_requests:
                with config.lock:
                    handle_serial_monitor_request(config, context)

            # Sleep briefly to avoid busy-wait
            time.sleep(0.5)

        except KeyboardInterrupt:
            # Check if operation is in progress - refuse to exit if so
            if context.status_manager.get_operation_in_progress():
                logging.warning("Received KeyboardInterrupt during active operation. Refusing to exit.")
                print(
                    f"\n⚠️  KeyboardInterrupt during operation\n⚠️  Cannot shutdown while operation is active\n⚠️  Use 'kill -9 {os.getpid()}' to force termination\n",
                    flush=True,
                )
                # Continue the main loop instead of exiting
                continue
            logging.warning("Daemon interrupted by user (no operation in progress)")
            _thread.interrupt_main()
            cleanup_and_exit(context)
        except Exception as e:
            logging.error(f"Daemon error: {e}", exc_info=True)
            # Continue running despite errors
            time.sleep(1)


def cleanup_pid_file(signum: int | None = None, frame: Any = None) -> None:
    """Cleanup handler to ensure PID file is removed on exit.

    This handler is registered for:
    - SIGTERM (graceful termination signal)
    - SIGINT (Ctrl+C)
    - atexit (normal program exit)

    Args:
        signum: Signal number (if called from signal handler)
        frame: Current stack frame (if called from signal handler)
    """
    try:
        if PID_FILE.exists():
            PID_FILE.unlink()
            logging.info("PID file cleaned up")
    except KeyboardInterrupt:
        raise
    except Exception as e:
        # Don't raise exceptions in signal handlers
        logging.error(f"Failed to cleanup PID file: {e}")


def parse_launcher_pid() -> int:
    """Parse --launched-by argument from command line.

    Returns:
        The PID of the client that launched this daemon.

    Raises:
        ValueError: If --launched-by argument is missing or invalid.
    """
    for arg in sys.argv:
        if arg.startswith("--launched-by="):
            try:
                return int(arg.split("=", 1)[1])
            except (ValueError, IndexError):
                raise ValueError(f"Invalid --launched-by argument: {arg}")
    raise ValueError("Missing required --launched-by argument")


def main() -> int:
    """
    Daemon main entry point.

    NOTE: This function NEVER spawns child processes. It runs in foreground mode only.
    All spawn logic is in singleton_manager.py (called by daemon API).

    The daemon is always launched with --launched-by=<PID> by the singleton manager.
    """
    # Parse arguments - launcher_pid is REQUIRED
    try:
        launcher_pid = parse_launcher_pid()
    except ValueError as e:
        print(f"Error: {e}", file=sys.stderr)
        print("Usage: python -m fbuild.daemon.daemon --launched-by=<PID>", file=sys.stderr)
        return 1

    # Setup logging FIRST
    setup_logging(foreground=True)  # Always foreground mode
    logging.info(f"Daemon starting (launched by PID {launcher_pid})")

    # Ensure daemon directory exists
    DAEMON_DIR.mkdir(parents=True, exist_ok=True)

    # Write PID file with launcher info (format: "daemon_pid,launcher_pid")
    PID_FILE.write_text(f"{os.getpid()},{launcher_pid}\n")
    logging.info(f"PID file written: {PID_FILE}")

    # Register cleanup handlers for graceful PID file removal
    # This ensures PID file is cleaned up on SIGTERM, SIGINT, and normal exit
    signal.signal(signal.SIGTERM, cleanup_pid_file)
    signal.signal(signal.SIGINT, cleanup_pid_file)
    atexit.register(cleanup_pid_file)
    logging.info("Registered cleanup handlers for SIGTERM, SIGINT, and atexit")

    # Run daemon loop
    try:
        run_daemon_loop()
    finally:
        # Cleanup (also called by signal handlers and atexit)
        if PID_FILE.exists():
            PID_FILE.unlink()
        logging.info("Daemon exiting")

    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt as ke:
        from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

        handle_keyboard_interrupt_properly(ke)
        print("\nDaemon interrupted by user")
        sys.exit(130)
