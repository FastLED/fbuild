"""
Zapio Daemon - Concurrent Deploy and Monitor Management

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

import json
import logging
import os
import signal
import subprocess
import sys
import threading
import time
from logging.handlers import RotatingFileHandler
from pathlib import Path
from typing import Any, overload

import psutil

from zapio.build import BuildOrchestratorAVR
from zapio.daemon.messages import (
    DaemonState,
    DaemonStatus,
    DeployRequest,
    MonitorRequest,
    OperationType,
)
from zapio.daemon.process_tracker import ProcessTracker
from zapio.deploy import ESP32Deployer
from zapio.deploy.monitor import SerialMonitor

# ============================================================================
# CONFIGURATION
# ============================================================================

DAEMON_NAME = "zapio_daemon"
DAEMON_DIR = Path.home() / ".zapio" / "daemon"
PID_FILE = DAEMON_DIR / f"{DAEMON_NAME}.pid"
STATUS_FILE = DAEMON_DIR / "daemon_status.json"
DEPLOY_REQUEST_FILE = DAEMON_DIR / "deploy_request.json"
MONITOR_REQUEST_FILE = DAEMON_DIR / "monitor_request.json"
LOG_FILE = DAEMON_DIR / "daemon.log"
PROCESS_REGISTRY_FILE = DAEMON_DIR / "process_registry.json"

# Timing constants
ORPHAN_CHECK_INTERVAL = 5  # Check for orphaned processes every 5 seconds
IDLE_TIMEOUT = 43200  # 12 hours
CANCEL_SIGNAL_CLEANUP_INTERVAL = 60  # Clean up stale cancel signals every 60s
CANCEL_SIGNAL_MAX_AGE = 300  # 5 minutes

# ============================================================================
# GLOBAL STATE
# ============================================================================

_daemon_pid: int | None = None
_daemon_started_at: float | None = None

# Lock management
_locks_lock = threading.Lock()  # Master lock for lock dictionaries
_port_locks: dict[str, threading.Lock] = {}  # Per-port locks for serial operations
_project_locks: dict[str, threading.Lock] = {}  # Per-project locks for builds
_operation_in_progress = False
_operation_lock = threading.Lock()


# ============================================================================
# LOGGING CONFIGURATION
# ============================================================================


def setup_logging(foreground: bool = False) -> None:
    """Setup logging for daemon with console and file handlers.

    Args:
        foreground: If True, also log to console (for debugging)
    """
    DAEMON_DIR.mkdir(parents=True, exist_ok=True)

    # Configure root logger
    logger = logging.getLogger()
    logger.setLevel(logging.INFO)

    # Console handler (for foreground mode)
    if foreground:
        console_handler = logging.StreamHandler(sys.stdout)
        console_handler.setLevel(logging.INFO)
        console_formatter = logging.Formatter(
            "%(asctime)s - %(name)s - %(levelname)s - %(message)s"
        )
        console_handler.setFormatter(console_formatter)
        logger.addHandler(console_handler)

    # Rotating file handler (always enabled)
    file_handler = RotatingFileHandler(
        str(LOG_FILE),
        maxBytes=10 * 1024 * 1024,  # 10MB
        backupCount=3,
    )
    file_handler.setLevel(logging.INFO)
    file_formatter = logging.Formatter(
        "%(asctime)s - %(name)s - %(levelname)s - %(message)s"
    )
    file_handler.setFormatter(file_formatter)
    logger.addHandler(file_handler)


# ============================================================================
# STATUS FILE MANAGEMENT
# ============================================================================


def read_status_file_safe() -> DaemonStatus:
    """Read status file with corruption recovery.

    Returns:
        DaemonStatus object (or default if corrupted)
    """
    default_status = DaemonStatus(
        state=DaemonState.IDLE,
        message="",
        updated_at=time.time(),
    )

    if not STATUS_FILE.exists():
        return default_status

    try:
        with open(STATUS_FILE) as f:
            data = json.load(f)
        return DaemonStatus.from_dict(data)

    except (json.JSONDecodeError, ValueError) as e:
        logging.warning(f"🔧 Corrupted status file detected: {e}")
        logging.warning("📝 Creating fresh status file")
        write_status_file_atomic(default_status.to_dict())
        return default_status
    except KeyboardInterrupt:
        raise
    except Exception as e:
        logging.error(f"❌ Unexpected error reading status file: {e}")
        write_status_file_atomic(default_status.to_dict())
        return default_status


def write_status_file_atomic(status: dict[str, Any]) -> None:
    """Write status file atomically to prevent corruption during writes.

    Uses atomic rename to ensure file is never left in a partial state.

    Args:
        status: Status dictionary to write
    """
    temp_file = STATUS_FILE.with_suffix(".tmp")

    try:
        with open(temp_file, "w") as f:
            json.dump(status, f, indent=2)

        # Atomic rename
        temp_file.replace(STATUS_FILE)

    except KeyboardInterrupt:
        temp_file.unlink(missing_ok=True)
        raise
    except Exception as e:
        logging.error(f"❌ Failed to write status file: {e}")
        temp_file.unlink(missing_ok=True)


def update_status(state: DaemonState, message: str, **kwargs: Any) -> None:
    """Update status file with current daemon state.

    Args:
        state: DaemonState enum value
        message: Human-readable status message
        **kwargs: Additional fields to include in status
    """
    global _daemon_pid, _daemon_started_at, _operation_in_progress

    status_obj = DaemonStatus(
        state=state,
        message=message,
        updated_at=time.time(),
        daemon_pid=_daemon_pid,
        daemon_started_at=_daemon_started_at,
        operation_in_progress=_operation_in_progress,
        **kwargs,
    )

    write_status_file_atomic(status_obj.to_dict())


# ============================================================================
# REQUEST FILE MANAGEMENT
# ============================================================================


@overload
def read_request_file(
    request_file: Path, request_class: type[DeployRequest]
) -> DeployRequest | None: ...


@overload
def read_request_file(
    request_file: Path, request_class: type[MonitorRequest]
) -> MonitorRequest | None: ...


def read_request_file(
    request_file: Path, request_class: type[DeployRequest] | type[MonitorRequest]
) -> DeployRequest | MonitorRequest | None:
    """Read and parse request file.

    Args:
        request_file: Path to request file
        request_class: Class to parse into (DeployRequest or MonitorRequest)

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
        logging.error(f"❌ Failed to parse request file {request_file}: {e}")
        return None
    except KeyboardInterrupt:
        raise
    except Exception as e:
        logging.error(f"❌ Unexpected error reading request file {request_file}: {e}")
        return None


def clear_request_file(request_file: Path) -> None:
    """Remove request file after processing.

    Args:
        request_file: Path to request file to clear
    """
    try:
        request_file.unlink(missing_ok=True)
    except KeyboardInterrupt:
        raise
    except Exception as e:
        logging.error(f"❌ Failed to clear request file {request_file}: {e}")


# ============================================================================
# LOCK MANAGEMENT
# ============================================================================


def get_port_lock(port: str) -> threading.Lock:
    """Get or create a lock for a specific serial port.

    Ensures only one operation can access a serial port at a time.

    Args:
        port: Serial port identifier

    Returns:
        Threading lock for this port
    """
    with _locks_lock:
        if port not in _port_locks:
            _port_locks[port] = threading.Lock()
        return _port_locks[port]


def get_project_lock(project_dir: str) -> threading.Lock:
    """Get or create a lock for a specific project directory.

    Ensures only one build operation can run per project at a time.

    Args:
        project_dir: Project directory path

    Returns:
        Threading lock for this project
    """
    with _locks_lock:
        if project_dir not in _project_locks:
            _project_locks[project_dir] = threading.Lock()
        return _project_locks[project_dir]


# ============================================================================
# DEPLOY REQUEST PROCESSING
# ============================================================================


def process_deploy_request(
    request: DeployRequest, process_tracker: ProcessTracker
) -> bool:
    """Execute deploy request with proper locking and error handling.

    Workflow:
    1. Acquire project lock (prevent concurrent builds)
    2. Acquire port lock (prevent concurrent serial access)
    3. Build firmware if clean_build requested
    4. Deploy firmware to device
    5. Start monitor if monitor_after requested

    Args:
        request: DeployRequest object
        process_tracker: ProcessTracker instance

    Returns:
        True if deployment successful, False otherwise
    """
    global _operation_in_progress

    project_dir = request.project_dir
    environment = request.environment
    port = request.port
    caller_pid = request.caller_pid
    caller_cwd = request.caller_cwd
    request_id = request.request_id

    logging.info(
        f"📥 Processing deploy request {request_id}: env={environment}, "
        + f"project={project_dir}, port={port}"
    )

    # Acquire project lock (prevent concurrent builds of same project)
    project_lock = get_project_lock(project_dir)

    # Try non-blocking acquire first to provide better feedback
    if not project_lock.acquire(blocking=False):
        logging.info(f"⏳ Project {project_dir} is busy, waiting for lock")
        update_status(
            DaemonState.DEPLOYING,
            f"⏳ Waiting for project {project_dir} (another build is in progress)",
            environment=environment,
            project_dir=project_dir,
            request_id=request_id,
            caller_pid=caller_pid,
            caller_cwd=caller_cwd,
        )

        # Now block and wait for the lock
        project_lock.acquire(blocking=True)
        logging.info(f"✅ Acquired project lock for {project_dir}")

    try:
        # Acquire port lock if port specified
        port_lock = None
        if port:
            port_lock = get_port_lock(port)
            if not port_lock.acquire(blocking=False):
                logging.info(f"⏳ Port {port} is busy, waiting for lock")
                update_status(
                    DaemonState.DEPLOYING,
                    f"⏳ Waiting for port {port} (in use by another operation)",
                    environment=environment,
                    project_dir=project_dir,
                    request_id=request_id,
                    caller_pid=caller_pid,
                    caller_cwd=caller_cwd,
                    port=port,
                )

                # Now block and wait for the lock
                port_lock.acquire(blocking=True)
                logging.info(f"✅ Acquired port lock for {port}")

        try:
            # Mark operation in progress
            with _operation_lock:
                _operation_in_progress = True

            update_status(
                DaemonState.DEPLOYING,
                f"🚀 Deploying {environment}",
                environment=environment,
                project_dir=project_dir,
                request_started_at=time.time(),
                request_id=request_id,
                caller_pid=caller_pid,
                caller_cwd=caller_cwd,
                operation_type=OperationType.DEPLOY,
                port=port,
            )

            # Build firmware if requested
            if request.clean_build:
                if not _execute_build(
                    project_dir, environment, request_id, caller_pid, caller_cwd, port
                ):
                    return False

            # Deploy firmware
            if not _execute_deploy(
                project_dir,
                environment,
                port,
                request_id,
                caller_pid,
                caller_cwd,
                request,
                process_tracker,
            ):
                return False

            return True

        finally:
            # Release port lock
            if port_lock:
                port_lock.release()

    finally:
        # Release project lock
        project_lock.release()

        # Mark operation complete
        with _operation_lock:
            _operation_in_progress = False


def _execute_build(
    project_dir: str,
    environment: str,
    request_id: str,
    caller_pid: int,
    caller_cwd: str,
    port: str | None,
) -> bool:
    """Execute build phase of deploy request.

    Args:
        project_dir: Project directory path
        environment: Build environment name
        request_id: Request ID
        caller_pid: Client PID
        caller_cwd: Client working directory
        port: Serial port (if applicable)

    Returns:
        True if build successful, False otherwise
    """
    logging.info(f"🔨 Building project: {project_dir}")
    update_status(
        DaemonState.BUILDING,
        f"🔨 Building {environment}",
        environment=environment,
        project_dir=project_dir,
        request_id=request_id,
        caller_pid=caller_pid,
        caller_cwd=caller_cwd,
        operation_type=OperationType.BUILD_AND_DEPLOY,
        port=port,
    )

    try:
        orchestrator = BuildOrchestratorAVR(verbose=False)
        build_result = orchestrator.build(
            project_dir=Path(project_dir),
            env_name=environment,
            clean=True,
            verbose=False,
        )

        if not build_result.success:
            logging.error(f"❌ Build failed: {build_result.message}")
            update_status(
                DaemonState.FAILED,
                f"❌ Build failed: {build_result.message}",
                exit_code=1,
                operation_in_progress=False,
            )
            return False

        logging.info("✅ Build completed successfully")
        return True

    except KeyboardInterrupt:
        raise
    except Exception as e:
        logging.error(f"❌ Build exception: {e}")
        update_status(
            DaemonState.FAILED,
            f"❌ Build exception: {e}",
            exit_code=1,
            operation_in_progress=False,
        )
        return False


def _execute_deploy(
    project_dir: str,
    environment: str,
    port: str | None,
    request_id: str,
    caller_pid: int,
    caller_cwd: str,
    request: DeployRequest,
    process_tracker: ProcessTracker,
) -> bool:
    """Execute deploy phase of deploy request.

    Args:
        project_dir: Project directory path
        environment: Build environment name
        port: Serial port (if applicable)
        request_id: Request ID
        caller_pid: Client PID
        caller_cwd: Client working directory
        request: Original DeployRequest
        process_tracker: ProcessTracker instance

    Returns:
        True if deploy successful, False otherwise
    """
    logging.info(f"📤 Deploying to {port if port else 'auto-detected port'}")
    update_status(
        DaemonState.DEPLOYING,
        f"📤 Deploying {environment}",
        environment=environment,
        project_dir=project_dir,
        request_id=request_id,
        caller_pid=caller_pid,
        caller_cwd=caller_cwd,
        operation_type=OperationType.DEPLOY,
        port=port,
    )

    try:
        deployer = ESP32Deployer(verbose=False)
        deploy_result = deployer.deploy(
            project_dir=Path(project_dir),
            env_name=environment,
            port=port,
        )

        if not deploy_result.success:
            logging.error(f"❌ Deploy failed: {deploy_result.message}")
            update_status(
                DaemonState.FAILED,
                f"❌ Deploy failed: {deploy_result.message}",
                exit_code=1,
                operation_in_progress=False,
            )
            return False

        logging.info("✅ Deploy completed successfully")
        used_port = deploy_result.port if deploy_result.port else port

        update_status(
            DaemonState.COMPLETED,
            "✅ Deploy successful",
            exit_code=0,
            operation_in_progress=False,
            port=used_port,
        )

        # Start monitor if requested
        if request.monitor_after and used_port:
            logging.info("📺 Starting monitor after successful deploy")

            monitor_request = MonitorRequest(
                project_dir=project_dir,
                environment=environment,
                port=used_port,
                baud_rate=None,  # Use config default
                halt_on_error=None,
                halt_on_success=None,
                timeout=request.monitor_timeout,
                caller_pid=caller_pid,
                caller_cwd=caller_cwd,
                request_id=f"monitor_after_{request_id}",
            )

            # Process monitor request immediately (blocks until complete/timeout)
            process_monitor_request(monitor_request, process_tracker)

        return True

    except KeyboardInterrupt:
        raise
    except Exception as e:
        logging.error(f"❌ Deploy exception: {e}")
        update_status(
            DaemonState.FAILED,
            f"❌ Deploy exception: {e}",
            exit_code=1,
            operation_in_progress=False,
        )
        return False


# ============================================================================
# MONITOR REQUEST PROCESSING
# ============================================================================


def process_monitor_request(
    request: MonitorRequest, process_tracker: ProcessTracker
) -> bool:
    """Execute monitor request with proper locking and error handling.

    Args:
        request: MonitorRequest object
        process_tracker: ProcessTracker instance

    Returns:
        True if monitoring successful, False otherwise
    """
    global _operation_in_progress

    project_dir = request.project_dir
    environment = request.environment
    port = request.port
    caller_pid = request.caller_pid
    caller_cwd = request.caller_cwd
    request_id = request.request_id

    logging.info(
        f"📥 Processing monitor request {request_id}: env={environment}, "
        + f"project={project_dir}, port={port}"
    )

    # Monitor requires port to be specified
    if not port:
        logging.error("❌ Monitor requires port to be specified")
        update_status(DaemonState.FAILED, "❌ Monitor requires port to be specified")
        return False

    # Acquire port lock (monitor requires exclusive port access)
    port_lock = get_port_lock(port)
    if not port_lock.acquire(blocking=False):
        logging.info(f"⏳ Port {port} is busy, waiting for lock")
        update_status(
            DaemonState.MONITORING,
            f"⏳ Waiting for port {port} (in use by another operation)",
            environment=environment,
            project_dir=project_dir,
            request_id=request_id,
            caller_pid=caller_pid,
            caller_cwd=caller_cwd,
            port=port,
        )

        # Now block and wait for the lock
        port_lock.acquire(blocking=True)
        logging.info(f"✅ Acquired port lock for {port}")

    try:
        # Mark operation in progress
        with _operation_lock:
            _operation_in_progress = True

        update_status(
            DaemonState.MONITORING,
            f"📺 Monitoring {environment} on {port}",
            environment=environment,
            project_dir=project_dir,
            request_started_at=time.time(),
            request_id=request_id,
            caller_pid=caller_pid,
            caller_cwd=caller_cwd,
            operation_type=OperationType.MONITOR,
            port=port,
        )

        # Start monitor
        logging.info(f"📺 Starting monitor on {port}")

        try:
            monitor = SerialMonitor(verbose=False)
            exit_code = monitor.monitor(
                project_dir=Path(project_dir),
                env_name=environment,
                port=port,
                baud=request.baud_rate if request.baud_rate else 115200,
                timeout=int(request.timeout) if request.timeout is not None else None,
                halt_on_error=request.halt_on_error,
                halt_on_success=request.halt_on_success,
            )

            if exit_code == 0:
                logging.info("✅ Monitor completed successfully")
                update_status(
                    DaemonState.COMPLETED,
                    "✅ Monitor completed",
                    exit_code=exit_code,
                    operation_in_progress=False,
                )
                return True
            else:
                logging.error(f"❌ Monitor failed with exit code {exit_code}")
                update_status(
                    DaemonState.FAILED,
                    f"❌ Monitor failed (exit {exit_code})",
                    exit_code=exit_code,
                    operation_in_progress=False,
                )
                return False

        except KeyboardInterrupt:
            raise
        except Exception as e:
            logging.error(f"❌ Monitor exception: {e}")
            update_status(
                DaemonState.FAILED,
                f"❌ Monitor exception: {e}",
                exit_code=1,
                operation_in_progress=False,
            )
            return False

    finally:
        # Release port lock
        port_lock.release()

        # Mark operation complete
        with _operation_lock:
            _operation_in_progress = False


# ============================================================================
# SIGNAL AND SHUTDOWN MANAGEMENT
# ============================================================================


def should_shutdown() -> bool:
    """Check if daemon should shutdown.

    Returns:
        True if shutdown signal detected, False otherwise
    """
    shutdown_file = DAEMON_DIR / "shutdown.signal"
    if shutdown_file.exists():
        logging.info("🛑 Shutdown signal detected")
        try:
            shutdown_file.unlink()
        except KeyboardInterrupt:
            raise
        except Exception:
            pass
        return True
    return False


def should_cancel_operation(request_id: str) -> bool:
    """Check if operation should be cancelled.

    Args:
        request_id: Request ID to check for cancellation

    Returns:
        True if cancel signal detected, False otherwise
    """
    cancel_file = DAEMON_DIR / f"cancel_{request_id}.signal"
    if cancel_file.exists():
        logging.info(f"🚫 Cancel signal detected for request {request_id}")
        try:
            cancel_file.unlink()
        except KeyboardInterrupt:
            raise
        except Exception:
            pass
        return True
    return False


def cleanup_stale_cancel_signals() -> None:
    """Clean up stale cancel signal files (older than 5 minutes)."""
    try:
        for signal_file in DAEMON_DIR.glob("cancel_*.signal"):
            try:
                file_age = time.time() - signal_file.stat().st_mtime
                if file_age > CANCEL_SIGNAL_MAX_AGE:
                    logging.info(
                        f"🧹 Cleaning up stale cancel signal: {signal_file.name}"
                    )
                    signal_file.unlink()
            except KeyboardInterrupt:
                raise
            except Exception as e:
                logging.warning(f"⚠️  Failed to clean up {signal_file.name}: {e}")
    except KeyboardInterrupt:
        raise
    except Exception as e:
        logging.error(f"❌ Error during cancel signal cleanup: {e}")


def signal_handler(signum: int, frame: object) -> None:
    """Handle SIGTERM/SIGINT - refuse shutdown during operation.

    Args:
        signum: Signal number
        frame: Current stack frame
    """
    global _operation_in_progress

    signal_name = "SIGTERM" if signum == signal.SIGTERM else "SIGINT"

    with _operation_lock:
        if _operation_in_progress:
            logging.warning(
                f"⚠️  Received {signal_name} during active operation. "
                + "Refusing graceful shutdown."
            )
            print(
                f"\n⚠️  {signal_name} received during operation\n"
                + "⚠️  Cannot shutdown gracefully while operation is active\n"
                + f"⚠️  Use 'kill -9 {os.getpid()}' to force termination\n",
                flush=True,
            )
            return  # Refuse shutdown
        else:
            logging.info(f"🛑 Received {signal_name}, shutting down gracefully")
            cleanup_and_exit()


def cleanup_and_exit() -> None:
    """Clean up daemon state and exit."""
    logging.info("🛑 Daemon shutting down")

    # Remove PID file
    try:
        PID_FILE.unlink(missing_ok=True)
    except KeyboardInterrupt:
        raise
    except Exception as e:
        logging.error(f"❌ Failed to remove PID file: {e}")

    # Set final status
    update_status(DaemonState.IDLE, "💤 Daemon shut down")

    sys.exit(0)


# ============================================================================
# MAIN DAEMON LOOP
# ============================================================================


def run_daemon_loop() -> None:
    """Main daemon loop: process deploy and monitor requests.

    The loop:
    1. Checks for shutdown signals
    2. Checks for idle timeout
    3. Periodically cleans up orphaned processes
    4. Processes deploy/monitor requests
    5. Sleeps briefly to avoid busy-waiting
    """
    global _daemon_pid, _daemon_started_at

    # Register signal handlers
    signal.signal(signal.SIGTERM, signal_handler)
    signal.signal(signal.SIGINT, signal_handler)

    # Initialize daemon tracking variables
    _daemon_pid = os.getpid()
    _daemon_started_at = time.time()

    # Initialize process tracker
    process_tracker = ProcessTracker(PROCESS_REGISTRY_FILE)

    logging.info(f"🚀 Daemon started with PID {_daemon_pid}")
    update_status(DaemonState.IDLE, "✅ Daemon ready")

    last_activity = time.time()
    last_orphan_check = time.time()
    last_cancel_cleanup = time.time()

    while True:
        try:
            # Check for shutdown signal
            if should_shutdown():
                cleanup_and_exit()

            # Check idle timeout
            if time.time() - last_activity > IDLE_TIMEOUT:
                logging.info(
                    f"⏰ Idle timeout reached ({IDLE_TIMEOUT}s), shutting down"
                )
                cleanup_and_exit()

            # Periodically check for and cleanup orphaned processes
            if time.time() - last_orphan_check >= ORPHAN_CHECK_INTERVAL:
                try:
                    orphaned_clients = process_tracker.cleanup_orphaned_processes()
                    if orphaned_clients:
                        logging.info(
                            "🧹 Cleaned up orphaned processes for "
                            + f"{len(orphaned_clients)} dead clients: {orphaned_clients}"
                        )
                    last_orphan_check = time.time()
                except KeyboardInterrupt:
                    raise
                except Exception as e:
                    logging.error(f"❌ Error during orphan cleanup: {e}", exc_info=True)

            # Periodically cleanup stale cancel signals
            if time.time() - last_cancel_cleanup >= CANCEL_SIGNAL_CLEANUP_INTERVAL:
                try:
                    cleanup_stale_cancel_signals()
                    last_cancel_cleanup = time.time()
                except KeyboardInterrupt:
                    raise
                except Exception as e:
                    logging.error(
                        f"❌ Error during cancel signal cleanup: {e}", exc_info=True
                    )

            # Check for deploy requests
            deploy_request = read_request_file(DEPLOY_REQUEST_FILE, DeployRequest)
            if deploy_request:
                last_activity = time.time()
                logging.info(f"📥 Received deploy request: {deploy_request}")

                # Process request
                process_deploy_request(deploy_request, process_tracker)

                # Clear request file
                clear_request_file(DEPLOY_REQUEST_FILE)

            # Check for monitor requests
            monitor_request = read_request_file(MONITOR_REQUEST_FILE, MonitorRequest)
            if monitor_request:
                last_activity = time.time()
                logging.info(f"📥 Received monitor request: {monitor_request}")

                # Process request
                process_monitor_request(monitor_request, process_tracker)

                # Clear request file
                clear_request_file(MONITOR_REQUEST_FILE)

            # Sleep briefly to avoid busy-wait
            time.sleep(0.5)

        except KeyboardInterrupt:
            logging.warning("⚠️  Daemon interrupted by user")
            cleanup_and_exit()
        except Exception as e:
            logging.error(f"❌ Daemon error: {e}", exc_info=True)
            # Continue running despite errors
            time.sleep(1)


# ============================================================================
# DAEMON INITIALIZATION
# ============================================================================


def main() -> int:
    """Main entry point for daemon.

    Returns:
        Exit code (0 for success)
    """
    # Parse command-line arguments
    foreground = "--foreground" in sys.argv

    # Setup logging
    setup_logging(foreground=foreground)

    # Ensure daemon directory exists
    DAEMON_DIR.mkdir(parents=True, exist_ok=True)

    if foreground:
        # Run in foreground (for debugging)
        logging.info("🔍 Running in foreground mode")
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
                logging.info(f"✅ Daemon already running with PID {existing_pid}")
                print(f"✅ Daemon already running with PID {existing_pid}")
                return 0
            else:
                # Stale PID file
                logging.info(f"🧹 Removing stale PID file for PID {existing_pid}")
                PID_FILE.unlink()
        except KeyboardInterrupt:
            raise
        except Exception as e:
            logging.warning(f"⚠️  Error checking existing PID: {e}")
            PID_FILE.unlink(missing_ok=True)

    # Simple daemonization for cross-platform compatibility
    try:
        # Fork to background
        if hasattr(os, "fork") and os.fork() > 0:  # type: ignore[attr-defined]
            # Parent process exits
            logging.info("🍴 Forked daemon to background")
            return 0
    except (OSError, AttributeError):
        # Fork not supported (Windows) - run in background as subprocess
        logging.info("🪟 Fork not supported (Windows), using subprocess")
        subprocess.Popen(
            [sys.executable, __file__, "--foreground"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            stdin=subprocess.DEVNULL,
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
    except KeyboardInterrupt:
        print("\n⚠️  Daemon interrupted by user")
        sys.exit(130)
