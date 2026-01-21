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
import json
import logging
import multiprocessing
import os
import signal
import subprocess
import sys
import threading
import time
from dataclasses import dataclass, field
from logging.handlers import TimedRotatingFileHandler
from pathlib import Path
from typing import Any, Callable, TypeVar

import psutil

from fbuild.daemon.compilation_queue import CompilationJobQueue
from fbuild.daemon.connection_registry import ConnectionRegistry
from fbuild.daemon.daemon_context import (
    DaemonContext,
    cleanup_daemon_context,
    create_daemon_context,
)
from fbuild.daemon.messages import (
    BuildRequest,
    DaemonState,
    DeployRequest,
    InstallDependenciesRequest,
    MonitorRequest,
)
from fbuild.daemon.process_tracker import ProcessTracker
from fbuild.daemon.processors.build_processor import BuildRequestProcessor
from fbuild.daemon.processors.deploy_processor import DeployRequestProcessor
from fbuild.daemon.processors.install_deps_processor import InstallDependenciesProcessor
from fbuild.daemon.processors.monitor_processor import MonitorRequestProcessor

# Type variable for request types
RequestT = TypeVar("RequestT", BuildRequest, DeployRequest, MonitorRequest, InstallDependenciesRequest)

# Module-level daemon context accessor for cross-module access
_daemon_context: DaemonContext | None = None

# Daemon configuration
DAEMON_NAME = "fbuild_daemon"

# Check for development mode (when running from repo)
if os.environ.get("FBUILD_DEV_MODE") == "1":
    # Use project-local daemon directory for development
    DAEMON_DIR = Path.cwd() / ".fbuild" / "daemon_dev"
else:
    # Use home directory for production
    DAEMON_DIR = Path.home() / ".fbuild" / "daemon"

PID_FILE = DAEMON_DIR / f"{DAEMON_NAME}.pid"
STATUS_FILE = DAEMON_DIR / "daemon_status.json"
BUILD_REQUEST_FILE = DAEMON_DIR / "build_request.json"
DEPLOY_REQUEST_FILE = DAEMON_DIR / "deploy_request.json"
MONITOR_REQUEST_FILE = DAEMON_DIR / "monitor_request.json"
INSTALL_DEPS_REQUEST_FILE = DAEMON_DIR / "install_deps_request.json"
LOG_FILE = DAEMON_DIR / "daemon.log"
PROCESS_REGISTRY_FILE = DAEMON_DIR / "process_registry.json"
FILE_CACHE_FILE = DAEMON_DIR / "file_cache.json"

# Device management request/response files
DEVICE_LIST_REQUEST_FILE = DAEMON_DIR / "device_list_request.json"
DEVICE_LIST_RESPONSE_FILE = DAEMON_DIR / "device_list_response.json"
DEVICE_STATUS_REQUEST_FILE = DAEMON_DIR / "device_status_request.json"
DEVICE_STATUS_RESPONSE_FILE = DAEMON_DIR / "device_status_response.json"
DEVICE_LEASE_REQUEST_FILE = DAEMON_DIR / "device_lease_request.json"
DEVICE_LEASE_RESPONSE_FILE = DAEMON_DIR / "device_lease_response.json"
DEVICE_RELEASE_REQUEST_FILE = DAEMON_DIR / "device_release_request.json"
DEVICE_RELEASE_RESPONSE_FILE = DAEMON_DIR / "device_release_response.json"
DEVICE_PREEMPT_REQUEST_FILE = DAEMON_DIR / "device_preempt_request.json"
DEVICE_PREEMPT_RESPONSE_FILE = DAEMON_DIR / "device_preempt_response.json"

# Connection management file patterns
CONNECTION_FILES_PATTERN = "connect_*.json"
HEARTBEAT_FILES_PATTERN = "heartbeat_*.json"
DISCONNECT_FILES_PATTERN = "disconnect_*.json"

ORPHAN_CHECK_INTERVAL = 5  # Check for orphaned processes every 5 seconds
STALE_LOCK_CHECK_INTERVAL = 60  # Check for stale locks every 60 seconds
DEAD_CLIENT_CHECK_INTERVAL = 10  # Check for dead clients every 10 seconds
IDLE_TIMEOUT = 43200  # 12 hours (fallback)
# Self-eviction timeout: if daemon has 0 clients AND 0 ops for this duration, shutdown
# Per TASK.md: "If daemon has 0 clients AND 0 running operations, immediately evict the daemon within 4 seconds."
SELF_EVICTION_TIMEOUT = 4.0  # 4 seconds


def get_compilation_queue() -> CompilationJobQueue | None:
    """Get the compilation queue from the daemon context.

    This function provides module-level access to the compilation queue for
    orchestrators and other components that need to submit compilation jobs.

    Returns:
        The compilation queue if the daemon is running, None otherwise.

    Example:
        >>> from fbuild.daemon import daemon
        >>> queue = daemon.get_compilation_queue()
        >>> if queue is not None:
        ...     queue.submit_job(compile_fn, args)
    """
    if _daemon_context is not None:
        return _daemon_context.compilation_queue
    return None


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


@dataclass
class RequestConfig:
    """Configuration for a request type in the daemon loop."""

    request_file: Path
    request_class: type
    processor: Any
    lock: threading.Lock = field(default_factory=threading.Lock)


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


def read_request_file(request_file: Path, request_class: type[RequestT]) -> RequestT | None:
    """Read and parse request file.

    Args:
        request_file: Path to request file
        request_class: Class to parse into (BuildRequest, DeployRequest, MonitorRequest, or InstallDependenciesRequest)

    Returns:
        Request object if valid, None otherwise
    """
    if not request_file.exists():
        return None

    try:
        with open(request_file) as f:
            data = json.load(f)
        return request_class.from_dict(data)
    except (json.JSONDecodeError, ValueError, TypeError) as e:
        logging.error(f"Failed to parse request file {request_file}: {e}")
        return None
    except KeyboardInterrupt:
        _thread.interrupt_main()
        raise
    except Exception as e:
        logging.error(f"Unexpected error reading request file {request_file}: {e}")
        return None


def clear_request_file(request_file: Path) -> None:
    """Remove request file after processing."""
    try:
        file_existed = request_file.exists()
        request_file.unlink(missing_ok=True)
        if file_existed:
            logging.debug(f"[ATOMIC_CONSUME] Successfully deleted request file: {request_file.name}")
        else:
            logging.warning(f"[ATOMIC_CONSUME] Request file already deleted: {request_file.name}")
    except KeyboardInterrupt:
        logging.warning(f"KeyboardInterrupt while clearing request file: {request_file}")
        _thread.interrupt_main()
        raise
    except Exception as e:
        logging.error(f"Failed to clear request file {request_file}: {e}")


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

    # Set final status
    context.status_manager.update_status(DaemonState.IDLE, "Daemon shut down")

    logging.info("Cleanup complete, exiting with status 0")
    sys.exit(0)


def handle_device_request(config: DeviceRequestConfig, context: DaemonContext) -> bool:
    """Handle a device request file if it exists.

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

        # Write response atomically
        temp_file = config.response_file.with_suffix(".tmp")
        with open(temp_file, "w") as f:
            json.dump(response_data, f, indent=2)
        temp_file.replace(config.response_file)

        return True

    except json.JSONDecodeError as e:
        logging.error(f"Invalid JSON in request file {config.request_file}: {e}")
        config.request_file.unlink(missing_ok=True)
        return False
    except KeyboardInterrupt:
        _thread.interrupt_main()
        raise
    except Exception as e:
        logging.error(f"Error handling device request {config.request_file}: {e}")
        try:
            with open(config.response_file, "w") as f:
                json.dump({"success": False, "message": str(e)}, f)
        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception:
            pass
        return False


def handle_device_list_request(request_data: dict[str, Any], context: DaemonContext) -> dict[str, Any]:
    """Handle device list request."""
    refresh = request_data.get("refresh", False)

    if refresh:
        context.device_manager.refresh_devices()

    devices = context.device_manager.get_all_devices()
    device_list = []

    for device_id, state in devices.items():
        device_list.append(
            {
                "device_id": device_id,
                "port": state.device_info.port,
                "is_connected": state.is_connected,
                "exclusive_holder": (state.exclusive_lease.client_id if state.exclusive_lease else None),
                "monitor_count": len(state.monitor_leases),
            }
        )

    logging.info(f"Device list request processed: {len(device_list)} devices")
    return {"success": True, "devices": device_list}


def handle_device_status_request(request_data: dict[str, Any], context: DaemonContext) -> dict[str, Any]:
    """Handle device status request."""
    device_id = request_data.get("device_id")
    if not device_id:
        return {"success": False, "message": "device_id is required"}

    status = context.device_manager.get_device_status(device_id)
    if not status.get("exists", False):
        return {"success": False, "message": f"Device {device_id} not found"}

    logging.info(f"Device status request processed for {device_id}")
    return {"success": True, **status}


def handle_device_lease_request(request_data: dict[str, Any], context: DaemonContext) -> dict[str, Any]:
    """Handle device lease request."""
    device_id = request_data.get("device_id")
    lease_type = request_data.get("lease_type", "exclusive")
    description = request_data.get("description", "")
    # Generate a client ID for file-based IPC clients (they don't have a persistent connection)
    client_id = request_data.get("client_id", f"file-ipc-{time.time()}")

    if not device_id:
        return {"success": False, "message": "device_id is required"}

    if lease_type == "monitor":
        lease = context.device_manager.acquire_monitor(
            device_id=device_id,
            client_id=client_id,
            description=description,
        )
    else:
        lease = context.device_manager.acquire_exclusive(
            device_id=device_id,
            client_id=client_id,
            description=description,
        )

    if lease is None:
        return {
            "success": False,
            "message": f"Failed to acquire {lease_type} lease on {device_id}",
        }

    logging.info(f"Device lease acquired: {lease_type} on {device_id} (lease_id={lease.lease_id})")
    return {"success": True, "lease_id": lease.lease_id, "client_id": client_id}


def handle_device_release_request(request_data: dict[str, Any], context: DaemonContext) -> dict[str, Any]:
    """Handle device release request."""
    device_id = request_data.get("device_id")
    client_id = request_data.get("client_id")

    if not device_id:
        return {"success": False, "message": "device_id is required"}

    # If device_id looks like a UUID, it might be a lease_id
    # Try to find the actual device and release by client
    state = context.device_manager.get_device(device_id)

    if state is None:
        # Try looking up by lease_id
        return {"success": False, "message": f"Device {device_id} not found"}

    # If client_id not provided, try to release any lease on this device
    # This is a simplification for file-based IPC where we don't track clients persistently
    if state.exclusive_lease:
        actual_client_id = client_id if client_id else state.exclusive_lease.client_id
        result = context.device_manager.release_lease(state.exclusive_lease.lease_id, actual_client_id)
        if result:
            logging.info(f"Released exclusive lease on {device_id}")
            return {"success": True, "message": f"Released exclusive lease on {device_id}"}

    return {"success": False, "message": f"No lease found to release on {device_id}"}


def handle_device_preempt_request(request_data: dict[str, Any], context: DaemonContext) -> dict[str, Any]:
    """Handle device preempt request."""
    device_id = request_data.get("device_id")
    reason = request_data.get("reason", "")
    client_id = request_data.get("client_id", f"file-ipc-{time.time()}")

    if not device_id:
        return {"success": False, "message": "device_id is required"}

    if not reason:
        return {"success": False, "message": "reason is required for preemption"}

    try:
        success, preempted_client_id = context.device_manager.preempt_device(
            device_id=device_id,
            requesting_client_id=client_id,
            reason=reason,
        )

        if success:
            # Get the new lease info
            state = context.device_manager.get_device(device_id)
            lease_id = state.exclusive_lease.lease_id if state and state.exclusive_lease else None

            logging.info(f"Device {device_id} preempted from {preempted_client_id} by {client_id}")
            return {
                "success": True,
                "preempted_client_id": preempted_client_id,
                "lease_id": lease_id,
                "client_id": client_id,
            }
        else:
            return {"success": False, "message": f"Failed to preempt device {device_id}"}

    except KeyboardInterrupt:
        _thread.interrupt_main()
        raise
    except Exception as e:
        logging.error(f"Error during device preemption: {e}")
        return {"success": False, "message": str(e)}


def process_operation_request(config: RequestConfig, context: DaemonContext) -> bool:
    """Process an operation request if one exists.

    Atomically consumes the request file and processes it.

    Args:
        config: Request configuration (file, class, processor, lock)
        context: Daemon context

    Returns:
        True if a request was processed, False otherwise
    """
    # Atomically read and clear request file under lock
    with config.lock:
        request = read_request_file(config.request_file, config.request_class)
        if request:
            clear_request_file(config.request_file)

    if not request:
        return False

    logging.info(f"Received {config.request_class.__name__}: {request}")

    # Mark operation in progress
    context.status_manager.set_operation_in_progress(True)
    try:
        config.processor.process_request(request, context)
    finally:
        context.status_manager.set_operation_in_progress(False)

    return True


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

    # Initialize process tracker
    process_tracker = ProcessTracker(PROCESS_REGISTRY_FILE)

    # Register signal handlers
    def signal_handler_wrapper(signum: int, frame: object) -> None:
        signal_handler(signum, frame, context)

    signal.signal(signal.SIGTERM, signal_handler_wrapper)
    signal.signal(signal.SIGINT, signal_handler_wrapper)

    # Configure operation request processors
    operation_requests = [
        RequestConfig(BUILD_REQUEST_FILE, BuildRequest, BuildRequestProcessor()),
        RequestConfig(DEPLOY_REQUEST_FILE, DeployRequest, DeployRequestProcessor()),
        RequestConfig(MONITOR_REQUEST_FILE, MonitorRequest, MonitorRequestProcessor()),
        RequestConfig(INSTALL_DEPS_REQUEST_FILE, InstallDependenciesRequest, InstallDependenciesProcessor()),
    ]

    # Configure device request handlers
    device_requests = [
        DeviceRequestConfig(DEVICE_LIST_REQUEST_FILE, DEVICE_LIST_RESPONSE_FILE, handle_device_list_request),
        DeviceRequestConfig(DEVICE_STATUS_REQUEST_FILE, DEVICE_STATUS_RESPONSE_FILE, handle_device_status_request),
        DeviceRequestConfig(DEVICE_LEASE_REQUEST_FILE, DEVICE_LEASE_RESPONSE_FILE, handle_device_lease_request),
        DeviceRequestConfig(DEVICE_RELEASE_REQUEST_FILE, DEVICE_RELEASE_RESPONSE_FILE, handle_device_release_request),
        DeviceRequestConfig(DEVICE_PREEMPT_REQUEST_FILE, DEVICE_PREEMPT_RESPONSE_FILE, handle_device_preempt_request),
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
        stale_count = len(stale_locks["port_locks"]) + len(stale_locks["project_locks"])
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
                    stale_count = len(stale_locks["port_locks"]) + len(stale_locks["project_locks"])
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

            # Process operation requests (build, deploy, monitor, install_deps)
            for config in operation_requests:
                if process_operation_request(config, context):
                    last_activity = time.time()

            # Process device management requests
            for config in device_requests:
                with config.lock:
                    handle_device_request(config, context)

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


def parse_spawner_pid() -> int | None:
    """Parse --spawned-by argument from command line.

    Returns:
        The PID of the client that spawned this daemon, or None if not provided.
    """
    for arg in sys.argv:
        if arg.startswith("--spawned-by="):
            try:
                return int(arg.split("=", 1)[1])
            except (ValueError, IndexError):
                return None
    return None


def main() -> int:
    """Main entry point for daemon."""
    # Parse command-line arguments
    foreground = "--foreground" in sys.argv
    spawner_pid = parse_spawner_pid()

    # Setup logging
    setup_logging(foreground=foreground)

    # Log spawner information immediately after logging setup
    if spawner_pid is not None:
        logging.info(f"Daemon spawned by client PID {spawner_pid}")
    else:
        logging.info("Daemon started without spawner info (manual start or legacy client)")

    # Ensure daemon directory exists
    DAEMON_DIR.mkdir(parents=True, exist_ok=True)

    if foreground:
        # Run in foreground (for debugging)
        logging.info("Running in foreground mode")
        # Write PID file
        with open(PID_FILE, "w") as f:
            f.write(str(os.getpid()))
        try:
            run_daemon_loop()
        finally:
            PID_FILE.unlink(missing_ok=True)
        return 0

    # Check if daemon already running
    if PID_FILE.exists():
        try:
            with open(PID_FILE) as f:
                existing_pid = int(f.read().strip())
            if psutil.pid_exists(existing_pid):
                logging.info(f"Daemon already running with PID {existing_pid}")
                print(f"Daemon already running with PID {existing_pid}")
                return 0
            else:
                # Stale PID file
                logging.info(f"Removing stale PID file for PID {existing_pid}")
                PID_FILE.unlink()
        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception as e:
            logging.warning(f"Error checking existing PID: {e}")
            PID_FILE.unlink(missing_ok=True)

    # Simple daemonization for cross-platform compatibility
    try:
        # Fork to background (Unix/Linux/macOS)
        if hasattr(os, "fork") and os.fork() > 0:  # type: ignore[attr-defined]
            # Parent process exits
            return 0
    except (OSError, AttributeError):
        # Fork not supported (Windows) - run in background as detached subprocess
        logging.info("Fork not supported (Windows), using detached subprocess")
        # Build command with spawner info if available
        cmd = [sys.executable, __file__, "--foreground"]
        if spawner_pid is not None:
            cmd.append(f"--spawned-by={spawner_pid}")

        # On Windows, use proper detachment flags:
        # - CREATE_NEW_PROCESS_GROUP: Isolates daemon from parent's Ctrl-C signals
        # - DETACHED_PROCESS: Daemon survives parent termination, no console inherited
        creationflags = 0
        if sys.platform == "win32":
            creationflags = subprocess.CREATE_NEW_PROCESS_GROUP | subprocess.DETACHED_PROCESS

        subprocess.Popen(
            cmd,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            stdin=subprocess.DEVNULL,
            cwd=str(DAEMON_DIR),
            creationflags=creationflags,
        )
        return 0

    # Child process continues
    # Write PID file
    with open(PID_FILE, "w") as f:
        f.write(str(os.getpid()))

    try:
        run_daemon_loop()
    finally:
        PID_FILE.unlink(missing_ok=True)

    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt as ke:
        from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

        handle_keyboard_interrupt_properly(ke)
        print("\nDaemon interrupted by user")
        sys.exit(130)
