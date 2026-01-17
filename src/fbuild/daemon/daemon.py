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
import logging
import multiprocessing
import os
import signal
import subprocess
import sys
import time
from logging.handlers import TimedRotatingFileHandler
from pathlib import Path

import psutil

from fbuild.daemon.daemon_context import (
    DaemonContext,
    cleanup_daemon_context,
    create_daemon_context,
)
from fbuild.daemon.messages import (
    BuildRequest,
    DaemonState,
    DeployRequest,
    MonitorRequest,
)
from fbuild.daemon.process_tracker import ProcessTracker
from fbuild.daemon.processors.build_processor import BuildRequestProcessor
from fbuild.daemon.processors.deploy_processor import DeployRequestProcessor
from fbuild.daemon.processors.monitor_processor import MonitorRequestProcessor

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


def setup_logging(foreground: bool = False) -> None:
    """Setup logging for daemon."""
    DAEMON_DIR.mkdir(parents=True, exist_ok=True)

    # Enhanced log format with function name and line number
    LOG_FORMAT = "%(asctime)s - %(name)s - %(levelname)s - [%(funcName)s:%(lineno)d] - %(message)s"
    LOG_DATEFMT = "%Y-%m-%d %H:%M:%S"

    # Configure root logger
    logger = logging.getLogger()
    logger.setLevel(logging.INFO)

    # Console handler (for foreground mode)
    if foreground:
        console_handler = logging.StreamHandler(sys.stdout)
        console_handler.setLevel(logging.INFO)
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
    file_handler.setLevel(logging.INFO)
    file_formatter = logging.Formatter(fmt=LOG_FORMAT, datefmt=LOG_DATEFMT)
    file_handler.setFormatter(file_formatter)
    logger.addHandler(file_handler)


def read_request_file(request_file: Path, request_class: type) -> BuildRequest | DeployRequest | MonitorRequest | None:
    """Read and parse request file.

    Args:
        request_file: Path to request file
        request_class: Class to parse into (BuildRequest, DeployRequest, or MonitorRequest)

    Returns:
        Request object if valid, None otherwise
    """
    import json

    if not request_file.exists():
        return None

    try:
        with open(request_file) as f:
            data = json.load(f)

        # Parse into typed request
        request = request_class.from_dict(data)
        return request

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

    # Write initial IDLE status IMMEDIATELY to prevent clients from reading stale status
    context.status_manager.update_status(DaemonState.IDLE, "Daemon starting...")

    # Initialize process tracker
    process_tracker = ProcessTracker(PROCESS_REGISTRY_FILE)

    # Register signal handlers
    def signal_handler_wrapper(signum: int, frame: object) -> None:
        signal_handler(signum, frame, context)

    signal.signal(signal.SIGTERM, signal_handler_wrapper)
    signal.signal(signal.SIGINT, signal_handler_wrapper)

    # Create request processors
    build_processor = BuildRequestProcessor()
    deploy_processor = DeployRequestProcessor()
    monitor_processor = MonitorRequestProcessor()

    logging.info(f"Daemon started with PID {daemon_pid}")
    context.status_manager.update_status(DaemonState.IDLE, "Daemon ready")

    last_activity = time.time()
    last_orphan_check = time.time()
    last_cancel_cleanup = time.time()

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

                # Mark operation in progress
                context.status_manager.set_operation_in_progress(True)

                # Process request
                build_processor.process_request(build_request, context)

                # Mark operation complete
                context.status_manager.set_operation_in_progress(False)

                # Clear request file
                clear_request_file(BUILD_REQUEST_FILE)

            # Check for deploy requests
            deploy_request = read_request_file(DEPLOY_REQUEST_FILE, DeployRequest)
            if deploy_request:
                last_activity = time.time()
                logging.info(f"Received deploy request: {deploy_request}")

                # Mark operation in progress
                context.status_manager.set_operation_in_progress(True)

                # Process request
                deploy_processor.process_request(deploy_request, context)

                # Mark operation complete
                context.status_manager.set_operation_in_progress(False)

                # Clear request file
                clear_request_file(DEPLOY_REQUEST_FILE)

            # Check for monitor requests
            monitor_request = read_request_file(MONITOR_REQUEST_FILE, MonitorRequest)
            if monitor_request:
                last_activity = time.time()
                logging.info(f"Received monitor request: {monitor_request}")

                # Mark operation in progress
                context.status_manager.set_operation_in_progress(True)

                # Process request
                monitor_processor.process_request(monitor_request, context)

                # Mark operation complete
                context.status_manager.set_operation_in_progress(False)

                # Clear request file
                clear_request_file(MONITOR_REQUEST_FILE)

            # Sleep briefly to avoid busy-wait
            time.sleep(0.5)

        except KeyboardInterrupt:
            logging.warning("Daemon interrupted by user")
            _thread.interrupt_main()
            cleanup_and_exit(context)
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
