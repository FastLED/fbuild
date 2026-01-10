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
import importlib
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
from typing import Any

import psutil

# Import modules (not classes) to enable proper reloading
from fbuild.daemon.compilation_queue import CompilationJobQueue
from fbuild.daemon.error_collector import ErrorCollector
from fbuild.daemon.file_cache import FileCache
from fbuild.daemon.messages import (
    BuildRequest,
    DaemonState,
    DaemonStatus,
    DeployRequest,
    MonitorRequest,
    OperationType,
)
from fbuild.daemon.operation_registry import OperationRegistry
from fbuild.daemon.process_tracker import ProcessTracker
from fbuild.daemon.subprocess_manager import SubprocessManager

# Daemon configuration
DAEMON_NAME = "fbuild_daemon"
DAEMON_DIR = Path.home() / ".fbuild" / "daemon"
PID_FILE = DAEMON_DIR / f"{DAEMON_NAME}.pid"
STATUS_FILE = DAEMON_DIR / "daemon_status.json"
BUILD_REQUEST_FILE = DAEMON_DIR / "build_request.json"
DEPLOY_REQUEST_FILE = DAEMON_DIR / "deploy_request.json"
MONITOR_REQUEST_FILE = DAEMON_DIR / "monitor_request.json"
LOG_FILE = DAEMON_DIR / "daemon.log"
PROCESS_REGISTRY_FILE = DAEMON_DIR / "process_registry.json"
FILE_CACHE_FILE = DAEMON_DIR / "file_cache.json"
ORPHAN_CHECK_INTERVAL = 5  # Check for orphaned processes every 5 seconds
IDLE_TIMEOUT = 43200  # 12 hours

# Global state
_daemon_pid: int | None = None
_daemon_started_at: float | None = None

# Daemon subsystems (initialized on daemon start)
_compilation_queue: CompilationJobQueue | None = None
_operation_registry: OperationRegistry | None = None
_subprocess_manager: SubprocessManager | None = None
_file_cache: FileCache | None = None
_error_collector: ErrorCollector | None = None

# Lock management
_locks_lock = threading.Lock()  # Master lock for lock dictionaries
_port_locks: dict[str, threading.Lock] = {}  # Per-port locks for serial operations
_project_locks: dict[str, threading.Lock] = {}  # Per-project locks for builds
_operation_in_progress = False
_operation_lock = threading.Lock()


def setup_logging(foreground: bool = False) -> None:
    """Setup logging for daemon."""
    DAEMON_DIR.mkdir(parents=True, exist_ok=True)

    # Configure root logger
    logger = logging.getLogger()
    logger.setLevel(logging.INFO)

    # Console handler (for foreground mode)
    if foreground:
        console_handler = logging.StreamHandler(sys.stdout)
        console_handler.setLevel(logging.INFO)
        console_formatter = logging.Formatter("%(asctime)s - %(name)s - %(levelname)s - %(message)s")
        console_handler.setFormatter(console_formatter)
        logger.addHandler(console_handler)

    # Rotating file handler (always)
    file_handler = RotatingFileHandler(
        str(LOG_FILE),
        maxBytes=10 * 1024 * 1024,  # 10MB
        backupCount=3,
    )
    file_handler.setLevel(logging.INFO)
    file_formatter = logging.Formatter("%(asctime)s - %(name)s - %(levelname)s - %(message)s")
    file_handler.setFormatter(file_formatter)
    logger.addHandler(file_handler)


def reload_build_modules() -> None:
    """Reload build-related modules to pick up code changes.

    This is critical for development on Windows where daemon caching prevents
    testing code changes. Reloads key modules that are frequently modified.

    Order matters: reload dependencies first, then modules that import them.
    """
    modules_to_reload = [
        # Core utilities and packages (reload first - no dependencies)
        "fbuild.packages.downloader",
        "fbuild.packages.archive_utils",
        "fbuild.packages.platformio_registry",
        "fbuild.packages.toolchain",
        "fbuild.packages.toolchain_esp32",
        "fbuild.packages.arduino_core",
        "fbuild.packages.framework_esp32",
        "fbuild.packages.platform_esp32",
        "fbuild.packages.library_manager",
        "fbuild.packages.library_manager_esp32",
        # Build system (reload second - depends on packages)
        "fbuild.build.archive_creator",
        "fbuild.build.compiler",
        "fbuild.build.configurable_compiler",
        "fbuild.build.linker",
        "fbuild.build.configurable_linker",
        "fbuild.build.source_scanner",
        "fbuild.build.compilation_executor",
        # Orchestrators (reload third - depends on build system)
        "fbuild.build.orchestrator",
        "fbuild.build.orchestrator_avr",
        "fbuild.build.orchestrator_esp32",
        # Deploy and monitor (reload with build system)
        "fbuild.deploy.deployer",
        "fbuild.deploy.deployer_esp32",
        "fbuild.deploy.monitor",
        # Top-level module packages (reload last to update __init__.py imports)
        "fbuild.build",
        "fbuild.deploy",
    ]

    reloaded_count = 0
    for module_name in modules_to_reload:
        if module_name in sys.modules:
            try:
                importlib.reload(sys.modules[module_name])
                reloaded_count += 1
                logging.debug(f"Reloaded module: {module_name}")
            except KeyboardInterrupt as ke:
                from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

                handle_keyboard_interrupt_properly(ke)
            except Exception as e:
                logging.warning(f"Failed to reload module {module_name}: {e}")

    if reloaded_count > 0:
        logging.info(f"Reloaded {reloaded_count} build modules")


def init_daemon_subsystems() -> None:
    """Initialize daemon subsystems (compilation queue, operation registry, etc.)."""
    global _compilation_queue, _operation_registry, _subprocess_manager, _file_cache, _error_collector

    logging.info("Initializing daemon subsystems...")

    # Initialize compilation queue with worker pool
    try:
        import multiprocessing

        num_workers = multiprocessing.cpu_count()
    except (ImportError, NotImplementedError):
        num_workers = 4  # Fallback for systems without multiprocessing

    _compilation_queue = CompilationJobQueue(num_workers=num_workers)
    _compilation_queue.start()
    logging.info(f"Compilation queue started with {num_workers} workers")

    # Initialize operation registry
    _operation_registry = OperationRegistry(max_history=100)
    logging.info("Operation registry initialized")

    # Initialize subprocess manager
    _subprocess_manager = SubprocessManager()
    logging.info("Subprocess manager initialized")

    # Initialize file cache
    _file_cache = FileCache(cache_file=FILE_CACHE_FILE)
    logging.info("File cache initialized")

    # Initialize error collector (created per-operation, but we can have a global one)
    _error_collector = ErrorCollector()
    logging.info("Error collector initialized")

    logging.info("✅ All daemon subsystems initialized successfully")


def shutdown_daemon_subsystems() -> None:
    """Shutdown daemon subsystems gracefully."""
    global _compilation_queue

    logging.info("Shutting down daemon subsystems...")

    if _compilation_queue:
        _compilation_queue.shutdown()
        logging.info("Compilation queue shut down")

    logging.info("✅ All daemon subsystems shut down")


def get_compilation_queue() -> CompilationJobQueue | None:
    """Get global compilation queue instance.

    Returns:
        CompilationJobQueue instance or None if not initialized
    """
    return _compilation_queue


def get_operation_registry() -> OperationRegistry | None:
    """Get global operation registry instance.

    Returns:
        OperationRegistry instance or None if not initialized
    """
    return _operation_registry


def get_subprocess_manager() -> SubprocessManager | None:
    """Get global subprocess manager instance.

    Returns:
        SubprocessManager instance or None if not initialized
    """
    return _subprocess_manager


def get_file_cache() -> FileCache | None:
    """Get global file cache instance.

    Returns:
        FileCache instance or None if not initialized
    """
    return _file_cache


def get_error_collector() -> ErrorCollector | None:
    """Get global error collector instance.

    Returns:
        ErrorCollector instance or None if not initialized
    """
    return _error_collector


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

        # Parse into typed DaemonStatus
        return DaemonStatus.from_dict(data)

    except (json.JSONDecodeError, ValueError) as e:
        logging.warning(f"Corrupted status file detected: {e}")
        logging.warning("Creating fresh status file")

        # Write fresh status file
        write_status_file_atomic(default_status.to_dict())

        return default_status
    except KeyboardInterrupt:
        _thread.interrupt_main()
        raise
    except Exception as e:
        logging.error(f"Unexpected error reading status file: {e}")
        write_status_file_atomic(default_status.to_dict())
        return default_status


def write_status_file_atomic(status: dict[str, Any]) -> None:
    """Write status file atomically to prevent corruption during writes.

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
        _thread.interrupt_main()
        temp_file.unlink(missing_ok=True)
        raise
    except Exception as e:
        logging.error(f"Failed to write status file: {e}")
        temp_file.unlink(missing_ok=True)


def update_status(state: DaemonState, message: str, **kwargs: Any) -> None:
    """Update status file with current daemon state.

    Args:
        state: DaemonState enum value
        message: Human-readable status message
        **kwargs: Additional fields to include in status
    """
    global _daemon_pid, _daemon_started_at, _operation_in_progress

    # Extract operation_in_progress from kwargs if provided to avoid duplicate keyword argument
    operation_in_progress = kwargs.pop("operation_in_progress", _operation_in_progress)

    # Create typed DaemonStatus object
    status_obj = DaemonStatus(
        state=state,
        message=message,
        updated_at=time.time(),
        daemon_pid=_daemon_pid,
        daemon_started_at=_daemon_started_at,
        operation_in_progress=operation_in_progress,
        **kwargs,
    )

    write_status_file_atomic(status_obj.to_dict())


def read_request_file(request_file: Path, request_class: type) -> Any:
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

        # Parse into typed request
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
        request_file.unlink(missing_ok=True)
    except KeyboardInterrupt:
        _thread.interrupt_main()
        raise
    except Exception as e:
        logging.error(f"Failed to clear request file {request_file}: {e}")


def get_port_lock(port: str) -> threading.Lock:
    """Get or create a lock for a specific serial port.

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

    Args:
        project_dir: Project directory path

    Returns:
        Threading lock for this project
    """
    with _locks_lock:
        if project_dir not in _project_locks:
            _project_locks[project_dir] = threading.Lock()
        return _project_locks[project_dir]


def process_deploy_request(request: DeployRequest, process_tracker: ProcessTracker) -> bool:
    """Execute deploy request.

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

    logging.info(f"Processing deploy request {request_id}: env={environment}, project={project_dir}, port={port}")

    # Acquire project lock (prevent concurrent builds of same project)
    project_lock = get_project_lock(project_dir)
    if not project_lock.acquire(blocking=False):
        logging.warning(f"Project {project_dir} is already being built")
        update_status(
            DaemonState.FAILED,
            f"Project {project_dir} is already being built by another process",
        )
        return False

    try:
        # Acquire port lock if port specified
        port_lock = None
        if port:
            port_lock = get_port_lock(port)
            if not port_lock.acquire(blocking=False):
                logging.warning(f"Port {port} is already in use")
                update_status(
                    DaemonState.FAILED,
                    f"Port {port} is already in use by another operation",
                )
                return False

        # Variables for post-deploy monitoring
        monitor_after = False
        monitor_request_data = None

        try:
            # Mark operation in progress
            with _operation_lock:
                _operation_in_progress = True

            update_status(
                DaemonState.DEPLOYING,
                f"Deploying {environment}",
                environment=environment,
                project_dir=project_dir,
                request_started_at=time.time(),
                request_id=request_id,
                caller_pid=caller_pid,
                caller_cwd=caller_cwd,
                operation_type=OperationType.DEPLOY,
                port=port,
            )

            # Build firmware (always build before deploy, incremental or clean)
            logging.info(f"Building project: {project_dir}")

            # Reload build modules to pick up code changes
            reload_build_modules()

            update_status(
                DaemonState.BUILDING,
                f"Building {environment}",
                environment=environment,
                project_dir=project_dir,
                request_id=request_id,
                caller_pid=caller_pid,
                caller_cwd=caller_cwd,
                operation_type=OperationType.BUILD_AND_DEPLOY,
                port=port,
            )

            try:
                # Get fresh class after module reload - must use getattr to get the reloaded class
                # Using fbuild.build.BuildOrchestratorAVR directly would use cached import
                orchestrator_class = getattr(sys.modules["fbuild.build.orchestrator_avr"], "BuildOrchestratorAVR")
                orchestrator = orchestrator_class(verbose=False)
                build_result = orchestrator.build(
                    project_dir=Path(project_dir),
                    env_name=environment,
                    clean=request.clean_build,
                    verbose=False,
                )

                if not build_result.success:
                    logging.error(f"Build failed: {build_result.message}")
                    update_status(
                        DaemonState.FAILED,
                        f"Build failed: {build_result.message}",
                        exit_code=1,
                        operation_in_progress=False,
                    )
                    return False

                logging.info("Build completed successfully")
            except KeyboardInterrupt:
                _thread.interrupt_main()
                raise
            except Exception as e:
                logging.error(f"Build exception: {e}")
                update_status(
                    DaemonState.FAILED,
                    f"Build exception: {e}",
                    exit_code=1,
                    operation_in_progress=False,
                )
                return False

            # Deploy firmware
            logging.info(f"Deploying to {port if port else 'auto-detected port'}")
            update_status(
                DaemonState.DEPLOYING,
                f"Deploying {environment}",
                environment=environment,
                project_dir=project_dir,
                request_id=request_id,
                caller_pid=caller_pid,
                caller_cwd=caller_cwd,
                operation_type=OperationType.DEPLOY,
                port=port,
            )

            try:
                # Get fresh class after module reload - must use getattr to get the reloaded class
                # Using fbuild.deploy.ESP32Deployer directly would use cached import
                deployer_class = getattr(sys.modules["fbuild.deploy.deployer_esp32"], "ESP32Deployer")
                deployer = deployer_class(verbose=False)
                deploy_result = deployer.deploy(
                    project_dir=Path(project_dir),
                    env_name=environment,
                    port=port,
                )

                if not deploy_result.success:
                    logging.error(f"Deploy failed: {deploy_result.message}")
                    update_status(
                        DaemonState.FAILED,
                        f"Deploy failed: {deploy_result.message}",
                        exit_code=1,
                        operation_in_progress=False,
                    )
                    return False

                logging.info("Deploy completed successfully")
                used_port = deploy_result.port if deploy_result.port else port

                # Store monitor request info before releasing locks
                monitor_after = request.monitor_after and used_port
                monitor_request_data = None

                if monitor_after:
                    # Create monitor request from deploy context
                    monitor_request_data = MonitorRequest(
                        project_dir=project_dir,
                        environment=environment,
                        port=used_port,
                        baud_rate=None,  # Use config default
                        halt_on_error=request.monitor_halt_on_error,
                        halt_on_success=request.monitor_halt_on_success,
                        expect=request.monitor_expect,
                        timeout=request.monitor_timeout,
                        caller_pid=caller_pid,
                        caller_cwd=caller_cwd,
                        request_id=f"monitor_after_{request_id}",
                    )
                else:
                    # No monitoring requested - mark deploy as completed
                    update_status(
                        DaemonState.COMPLETED,
                        "Deploy successful",
                        exit_code=0,
                        operation_in_progress=False,
                        port=used_port,
                    )

            except KeyboardInterrupt:
                _thread.interrupt_main()
                raise
            except Exception as e:
                logging.error(f"Deploy exception: {e}")
                update_status(
                    DaemonState.FAILED,
                    f"Deploy exception: {e}",
                    exit_code=1,
                    operation_in_progress=False,
                )
                return False

        finally:
            # Release port lock
            if port_lock:
                port_lock.release()

        # Start monitor if requested (after releasing port lock to avoid deadlock)
        if monitor_after and monitor_request_data:
            logging.info("Starting monitor after successful deploy")

            # Update status to indicate we're transitioning to monitor
            # This prevents the client from seeing COMPLETED before monitoring starts
            update_status(
                DaemonState.MONITORING,
                "Transitioning to monitor after deploy",
                environment=monitor_request_data.environment,
                project_dir=monitor_request_data.project_dir,
            )

            # Process monitor request immediately
            # Note: This blocks until monitor completes/times out
            # The monitor will set final COMPLETED/FAILED status
            process_monitor_request(monitor_request_data, process_tracker)

        return True

    finally:
        # Release project lock
        project_lock.release()

        # Mark operation complete
        with _operation_lock:
            _operation_in_progress = False


def process_monitor_request(request: MonitorRequest, process_tracker: ProcessTracker) -> bool:
    """Execute monitor request.

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

    logging.info(f"Processing monitor request {request_id}: env={environment}, project={project_dir}, port={port}")

    # Acquire port lock (monitor requires exclusive port access)
    if not port:
        logging.error("Monitor requires port to be specified")
        update_status(DaemonState.FAILED, "Monitor requires port to be specified")
        return False

    port_lock = get_port_lock(port)
    if not port_lock.acquire(blocking=False):
        logging.warning(f"Port {port} is already in use")
        update_status(
            DaemonState.FAILED,
            f"Port {port} is already in use by another operation",
        )
        return False

    try:
        # Mark operation in progress
        with _operation_lock:
            _operation_in_progress = True

        update_status(
            DaemonState.MONITORING,
            f"Monitoring {environment} on {port}",
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
        logging.info(f"Starting monitor on {port}")

        # Create output file path for streaming
        output_file = Path(project_dir) / ".fbuild" / "monitor_output.txt"
        output_file.parent.mkdir(parents=True, exist_ok=True)
        # Clear/truncate output file before starting
        output_file.write_text("", encoding="utf-8")

        # Create summary file path
        summary_file = Path(project_dir) / ".fbuild" / "monitor_summary.json"
        # Clear old summary file
        if summary_file.exists():
            summary_file.unlink()

        try:
            # Get fresh class after module reload - must use getattr to get the reloaded class
            # Using fbuild.deploy.monitor.SerialMonitor directly would use cached import
            monitor_class = getattr(sys.modules["fbuild.deploy.monitor"], "SerialMonitor")
            monitor = monitor_class(verbose=False)
            exit_code = monitor.monitor(
                project_dir=Path(project_dir),
                env_name=environment,
                port=port,
                baud=request.baud_rate if request.baud_rate else 115200,
                timeout=int(request.timeout) if request.timeout is not None else None,
                halt_on_error=request.halt_on_error,
                halt_on_success=request.halt_on_success,
                expect=request.expect,
                output_file=output_file,
                summary_file=summary_file,
            )

            if exit_code == 0:
                logging.info("Monitor completed successfully")
                update_status(
                    DaemonState.COMPLETED,
                    "Monitor completed",
                    exit_code=exit_code,
                    operation_in_progress=False,
                )
                return True
            else:
                logging.error(f"Monitor failed with exit code {exit_code}")
                update_status(
                    DaemonState.FAILED,
                    f"Monitor failed (exit {exit_code})",
                    exit_code=exit_code,
                    operation_in_progress=False,
                )
                return False

        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception as e:
            logging.error(f"Monitor exception: {e}")
            update_status(
                DaemonState.FAILED,
                f"Monitor exception: {e}",
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


def process_build_request(request: BuildRequest, process_tracker: ProcessTracker) -> bool:
    """Execute build request.

    Args:
        request: BuildRequest object
        process_tracker: ProcessTracker instance

    Returns:
        True if build successful, False otherwise
    """
    global _operation_in_progress

    project_dir = request.project_dir
    environment = request.environment
    caller_pid = request.caller_pid
    caller_cwd = request.caller_cwd
    request_id = request.request_id
    clean_build = request.clean_build
    verbose = request.verbose

    logging.info(f"Processing build request {request_id}: env={environment}, project={project_dir}, clean={clean_build}")

    # Acquire project lock (prevent concurrent builds of same project)
    project_lock = get_project_lock(project_dir)
    if not project_lock.acquire(blocking=False):
        logging.warning(f"Project {project_dir} is already being built")
        update_status(
            DaemonState.FAILED,
            f"Project {project_dir} is already being built by another process",
        )
        return False

    try:
        # Mark operation in progress
        with _operation_lock:
            _operation_in_progress = True

        update_status(
            DaemonState.BUILDING,
            f"Building {environment}",
            environment=environment,
            project_dir=project_dir,
            request_started_at=time.time(),
            request_id=request_id,
            caller_pid=caller_pid,
            caller_cwd=caller_cwd,
            operation_type=OperationType.BUILD,
        )

        # Execute build
        logging.info(f"Building project: {project_dir}")

        # Reload build modules to pick up code changes
        reload_build_modules()

        try:
            # Get fresh class after module reload - must use getattr to get the reloaded class
            # Using fbuild.build.BuildOrchestratorAVR directly would use cached import
            orchestrator_class = getattr(sys.modules["fbuild.build.orchestrator_avr"], "BuildOrchestratorAVR")
            orchestrator = orchestrator_class(verbose=verbose)
            build_result = orchestrator.build(
                project_dir=Path(project_dir),
                env_name=environment,
                clean=clean_build,
                verbose=verbose,
            )

            if not build_result.success:
                logging.error(f"Build failed: {build_result.message}")
                update_status(
                    DaemonState.FAILED,
                    f"Build failed: {build_result.message}",
                    exit_code=1,
                    operation_in_progress=False,
                )
                return False

            logging.info("Build completed successfully")
            update_status(
                DaemonState.COMPLETED,
                "Build successful",
                exit_code=0,
                operation_in_progress=False,
            )
            return True

        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception as e:
            logging.error(f"Build exception: {e}")
            update_status(
                DaemonState.FAILED,
                f"Build exception: {e}",
                exit_code=1,
                operation_in_progress=False,
            )
            return False

    finally:
        # Release project lock
        project_lock.release()

        # Mark operation complete
        with _operation_lock:
            _operation_in_progress = False


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
    # Check for cancel signal file
    cancel_file = DAEMON_DIR / f"cancel_{request_id}.signal"
    if cancel_file.exists():
        logging.info(f"Cancel signal detected for request {request_id}")
        try:
            cancel_file.unlink()
        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception:
            pass
        return True
    return False


def signal_handler(signum: int, frame: object) -> None:
    """Handle SIGTERM/SIGINT - refuse shutdown during operation."""
    global _operation_in_progress

    signal_name = "SIGTERM" if signum == signal.SIGTERM else "SIGINT"

    with _operation_lock:
        if _operation_in_progress:
            logging.warning(f"Received {signal_name} during active operation. Refusing graceful shutdown.")
            print(
                f"\n⚠️  {signal_name} received during operation\n⚠️  Cannot shutdown gracefully while operation is active\n⚠️  Use 'kill -9 {os.getpid()}' to force termination\n",
                flush=True,
            )
            return  # Refuse shutdown
        else:
            logging.info(f"Received {signal_name}, shutting down gracefully")
            cleanup_and_exit()


def cleanup_and_exit() -> None:
    """Clean up daemon state and exit."""
    logging.info("Daemon shutting down")

    # Shutdown subsystems
    shutdown_daemon_subsystems()

    # Remove PID file
    try:
        PID_FILE.unlink(missing_ok=True)
    except KeyboardInterrupt:
        _thread.interrupt_main()
        raise
    except Exception as e:
        logging.error(f"Failed to remove PID file: {e}")

    # Set final status
    update_status(DaemonState.IDLE, "Daemon shut down")

    sys.exit(0)


def cleanup_stale_cancel_signals() -> None:
    """Clean up stale cancel signal files (older than 5 minutes)."""
    try:
        for signal_file in DAEMON_DIR.glob("cancel_*.signal"):
            try:
                # Check file age
                file_age = time.time() - signal_file.stat().st_mtime
                if file_age > 300:  # 5 minutes
                    logging.info(f"Cleaning up stale cancel signal: {signal_file.name}")
                    signal_file.unlink()
            except KeyboardInterrupt:
                _thread.interrupt_main()
                raise
            except Exception as e:
                logging.warning(f"Failed to clean up {signal_file.name}: {e}")
    except KeyboardInterrupt:
        _thread.interrupt_main()
        raise
    except Exception as e:
        logging.error(f"Error during cancel signal cleanup: {e}")


def run_daemon_loop() -> None:
    """Main daemon loop: process deploy and monitor requests."""
    global _daemon_pid, _daemon_started_at

    # Register signal handlers
    signal.signal(signal.SIGTERM, signal_handler)
    signal.signal(signal.SIGINT, signal_handler)

    # Initialize daemon tracking variables
    _daemon_pid = os.getpid()
    _daemon_started_at = time.time()

    # Write initial IDLE status IMMEDIATELY to prevent clients from reading stale status
    # This must happen before processing any requests to ensure clean state
    update_status(DaemonState.IDLE, "Daemon starting...")

    # Initialize daemon subsystems
    init_daemon_subsystems()

    # Initialize process tracker
    process_tracker = ProcessTracker(PROCESS_REGISTRY_FILE)

    logging.info(f"Daemon started with PID {_daemon_pid}")
    update_status(DaemonState.IDLE, "Daemon ready")

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
                logging.info(f"Idle timeout reached ({IDLE_TIMEOUT}s), shutting down")
                cleanup_and_exit()

            # Periodically check for and cleanup orphaned processes
            if time.time() - last_orphan_check >= ORPHAN_CHECK_INTERVAL:
                try:
                    orphaned_clients = process_tracker.cleanup_orphaned_processes()
                    if orphaned_clients:
                        logging.info(f"Cleaned up orphaned processes for {len(orphaned_clients)} dead clients: {orphaned_clients}")
                    last_orphan_check = time.time()
                except KeyboardInterrupt:
                    _thread.interrupt_main()
                    raise
                except Exception as e:
                    logging.error(f"Error during orphan cleanup: {e}", exc_info=True)

            # Periodically cleanup stale cancel signals (every 60 seconds)
            if time.time() - last_cancel_cleanup >= 60:
                try:
                    cleanup_stale_cancel_signals()
                    last_cancel_cleanup = time.time()
                except KeyboardInterrupt:
                    _thread.interrupt_main()
                    raise
                except Exception as e:
                    logging.error(f"Error during cancel signal cleanup: {e}", exc_info=True)

            # Check for build requests
            build_request = read_request_file(BUILD_REQUEST_FILE, BuildRequest)
            if build_request:
                last_activity = time.time()
                logging.info(f"Received build request: {build_request}")

                # Process request
                process_build_request(build_request, process_tracker)

                # Clear request file
                clear_request_file(BUILD_REQUEST_FILE)

            # Check for deploy requests
            deploy_request = read_request_file(DEPLOY_REQUEST_FILE, DeployRequest)
            if deploy_request:
                last_activity = time.time()
                logging.info(f"Received deploy request: {deploy_request}")

                # Process request
                process_deploy_request(deploy_request, process_tracker)

                # Clear request file
                clear_request_file(DEPLOY_REQUEST_FILE)

            # Check for monitor requests
            monitor_request = read_request_file(MONITOR_REQUEST_FILE, MonitorRequest)
            if monitor_request:
                last_activity = time.time()
                logging.info(f"Received monitor request: {monitor_request}")

                # Process request
                process_monitor_request(monitor_request, process_tracker)

                # Clear request file
                clear_request_file(MONITOR_REQUEST_FILE)

            # Sleep briefly to avoid busy-wait
            time.sleep(0.5)

        except KeyboardInterrupt:  # noqa: KBI002
            logging.warning("Daemon interrupted by user")
            cleanup_and_exit()
        except Exception as e:
            logging.error(f"Daemon error: {e}", exc_info=True)
            # Continue running despite errors
            time.sleep(1)


def main() -> int:
    """Main entry point for daemon."""
    # Parse command-line arguments
    foreground = "--foreground" in sys.argv

    # Setup logging
    setup_logging(foreground=foreground)

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
        # Fork to background
        if hasattr(os, "fork") and os.fork() > 0:  # type: ignore[attr-defined]
            # Parent process exits
            return 0
    except (OSError, AttributeError):
        # Fork not supported (Windows) - run in background as subprocess
        logging.info("Fork not supported, using subprocess")
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
    except KeyboardInterrupt:  # noqa: KBI002
        print("\nDaemon interrupted by user")
        sys.exit(130)
