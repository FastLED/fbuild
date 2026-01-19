"""
Daemon Context - Centralized state management for fbuild daemon.

This module provides the DaemonContext class which encapsulates all daemon state
that was previously stored in global variables. This improves testability,
makes dependencies explicit, and eliminates global mutable state.
"""

import threading
from dataclasses import dataclass, field
from pathlib import Path

from fbuild.daemon.async_client import ClientConnectionManager
from fbuild.daemon.compilation_queue import CompilationJobQueue
from fbuild.daemon.configuration_lock import ConfigurationLockManager
from fbuild.daemon.error_collector import ErrorCollector
from fbuild.daemon.file_cache import FileCache
from fbuild.daemon.firmware_ledger import FirmwareLedger
from fbuild.daemon.lock_manager import ResourceLockManager
from fbuild.daemon.operation_registry import OperationRegistry
from fbuild.daemon.port_state_manager import PortStateManager
from fbuild.daemon.shared_serial import SharedSerialManager
from fbuild.daemon.status_manager import StatusManager
from fbuild.daemon.subprocess_manager import SubprocessManager


@dataclass
class DaemonContext:
    """Centralized context for all daemon state and subsystems.

    This class replaces the 12 global variables in daemon.py with a single
    context object that can be passed to functions explicitly. This improves:
    - Testability: Mock the entire context in tests
    - Clarity: Dependencies are explicit in function signatures
    - Thread-safety: Locks are properly encapsulated
    - Lifecycle: Cleanup is centralized in one place

    Attributes:
        daemon_pid: Process ID of the daemon
        daemon_started_at: Unix timestamp when daemon was started
        compilation_queue: Queue for managing parallel compilation jobs
        operation_registry: Registry for tracking active/completed operations
        subprocess_manager: Manager for daemon-spawned subprocesses
        file_cache: Cache for file modification times
        error_collector: Global error collector for operations
        lock_manager: Unified resource lock manager for ports and projects
        port_state_manager: Manager for tracking COM port states
        status_manager: Manager for daemon status file operations
        client_manager: Manager for async client connections with heartbeat
        configuration_lock_manager: Centralized locking for (project, env, port) configs
        firmware_ledger: Tracks deployed firmware on devices to avoid re-upload
        shared_serial_manager: Manages shared serial port access for multiple clients
        operation_in_progress: Flag indicating if any operation is running
        operation_lock: Lock protecting the operation_in_progress flag
    """

    # Daemon identity
    daemon_pid: int
    daemon_started_at: float

    # Subsystems
    compilation_queue: CompilationJobQueue
    operation_registry: OperationRegistry
    subprocess_manager: SubprocessManager
    file_cache: FileCache
    error_collector: ErrorCollector
    lock_manager: ResourceLockManager
    port_state_manager: PortStateManager
    status_manager: StatusManager

    # New managers for centralized locking and shared state (Iteration 1-2)
    client_manager: ClientConnectionManager
    configuration_lock_manager: ConfigurationLockManager
    firmware_ledger: FirmwareLedger
    shared_serial_manager: SharedSerialManager

    # Operation state
    operation_in_progress: bool = False
    operation_lock: threading.Lock = field(default_factory=threading.Lock)


def create_daemon_context(
    daemon_pid: int,
    daemon_started_at: float,
    num_workers: int,
    file_cache_path: Path,
    status_file_path: Path,
) -> DaemonContext:
    """Factory function to create and initialize a DaemonContext.

    This function initializes all daemon subsystems and returns a fully
    configured DaemonContext ready for use.

    Args:
        daemon_pid: Process ID of the daemon
        daemon_started_at: Unix timestamp when daemon started
        num_workers: Number of compilation worker threads
        file_cache_path: Path to the file cache JSON file
        status_file_path: Path to the status file

    Returns:
        Fully initialized DaemonContext

    Example:
        >>> import os
        >>> import time
        >>> from pathlib import Path
        >>>
        >>> context = create_daemon_context(
        ...     daemon_pid=os.getpid(),
        ...     daemon_started_at=time.time(),
        ...     num_workers=4,
        ...     file_cache_path=Path.home() / ".fbuild" / "daemon" / "file_cache.json",
        ...     status_file_path=Path.home() / ".fbuild" / "daemon" / "daemon_status.json"
        ... )
        >>> # Use context in request handlers
        >>> process_build_request(request, context)
    """
    import logging

    logging.info("Initializing daemon context...")

    # Initialize compilation queue with worker pool
    compilation_queue = CompilationJobQueue(num_workers=num_workers)
    compilation_queue.start()
    logging.info(f"Compilation queue started with {num_workers} workers")

    # Initialize operation registry
    logging.debug("Creating operation registry (max_history=100)...")
    operation_registry = OperationRegistry(max_history=100)
    logging.info("Operation registry initialized")

    # Initialize subprocess manager
    subprocess_manager = SubprocessManager()
    logging.info("Subprocess manager initialized")

    # Initialize file cache
    logging.debug(f"Creating file cache (cache_file={file_cache_path})...")
    file_cache = FileCache(cache_file=file_cache_path)
    logging.info("File cache initialized")

    # Initialize error collector
    error_collector = ErrorCollector()
    logging.info("Error collector initialized")

    # Initialize lock manager
    lock_manager = ResourceLockManager()
    logging.info("Resource lock manager initialized")

    # Initialize port state manager
    port_state_manager = PortStateManager()
    logging.info("Port state manager initialized")

    # Initialize status manager (with port state manager and lock manager for status visibility)
    logging.debug(f"Creating status manager (status_file={status_file_path})...")
    status_manager = StatusManager(
        status_file=status_file_path,
        daemon_pid=daemon_pid,
        daemon_started_at=daemon_started_at,
        port_state_manager=port_state_manager,
        lock_manager=lock_manager,
    )
    logging.info("Status manager initialized")

    # Initialize new managers for centralized locking and shared state (Iteration 1-2)
    client_manager = ClientConnectionManager()
    logging.info("Client connection manager initialized")

    configuration_lock_manager = ConfigurationLockManager()
    logging.info("Configuration lock manager initialized")

    firmware_ledger = FirmwareLedger()
    logging.info(f"Firmware ledger initialized (path={firmware_ledger.ledger_path})")

    shared_serial_manager = SharedSerialManager()
    logging.info("Shared serial manager initialized")

    # Register cleanup callbacks: when a client disconnects, release their resources
    def on_client_disconnect(client_id: str) -> None:
        """Cleanup callback for when a client disconnects."""
        logging.info(f"Cleaning up resources for disconnected client: {client_id}")
        # Release all configuration locks held by this client
        released = configuration_lock_manager.release_all_client_locks(client_id)
        if released > 0:
            logging.info(f"Released {released} configuration locks for client {client_id}")
        # Disconnect from shared serial sessions
        shared_serial_manager.disconnect_client(client_id)

    client_manager.register_cleanup_callback(on_client_disconnect)
    logging.info("Client cleanup callback registered")

    # Create context
    context = DaemonContext(
        daemon_pid=daemon_pid,
        daemon_started_at=daemon_started_at,
        compilation_queue=compilation_queue,
        operation_registry=operation_registry,
        subprocess_manager=subprocess_manager,
        file_cache=file_cache,
        error_collector=error_collector,
        lock_manager=lock_manager,
        port_state_manager=port_state_manager,
        status_manager=status_manager,
        client_manager=client_manager,
        configuration_lock_manager=configuration_lock_manager,
        firmware_ledger=firmware_ledger,
        shared_serial_manager=shared_serial_manager,
    )

    logging.info("✅ Daemon context initialized successfully")
    return context


def cleanup_daemon_context(context: DaemonContext) -> None:
    """Cleanup and shutdown all daemon subsystems in the context.

    This function should be called during daemon shutdown to ensure all
    resources are properly released.

    Args:
        context: The DaemonContext to clean up

    Example:
        >>> try:
        ...     run_daemon(context)
        ... finally:
        ...     cleanup_daemon_context(context)
    """
    import logging

    logging.info("Shutting down daemon context...")

    # Shutdown shared serial manager first (closes all serial ports)
    if context.shared_serial_manager:
        try:
            context.shared_serial_manager.shutdown()
            logging.info("Shared serial manager shut down")
        except KeyboardInterrupt:  # noqa: KBI002
            logging.warning("KeyboardInterrupt during shared serial manager shutdown")
            raise
        except Exception as e:
            logging.error(f"Error shutting down shared serial manager: {e}")

    # Clear all configuration locks
    if context.configuration_lock_manager:
        try:
            cleared = context.configuration_lock_manager.clear_all_locks()
            logging.info(f"Cleared {cleared} configuration locks during shutdown")
        except KeyboardInterrupt:  # noqa: KBI002
            logging.warning("KeyboardInterrupt during configuration lock manager cleanup")
            raise
        except Exception as e:
            logging.error(f"Error clearing configuration locks: {e}")

    # Clear all client connections
    if context.client_manager:
        try:
            cleared = context.client_manager.clear_all_clients()
            logging.info(f"Cleared {cleared} client connections during shutdown")
        except KeyboardInterrupt:  # noqa: KBI002
            logging.warning("KeyboardInterrupt during client manager cleanup")
            raise
        except Exception as e:
            logging.error(f"Error clearing client connections: {e}")

    # Shutdown compilation queue
    if context.compilation_queue:
        try:
            context.compilation_queue.shutdown()
            logging.info("Compilation queue shut down")
        except KeyboardInterrupt:  # noqa: KBI002
            logging.warning("KeyboardInterrupt during compilation queue shutdown")
            raise
        except Exception as e:
            logging.error(f"Error shutting down compilation queue: {e}")

    # Clear all locks during shutdown
    if context.lock_manager:
        try:
            cleared = context.lock_manager.clear_all_locks()
            logging.info(f"Cleared {cleared} locks during shutdown")
        except KeyboardInterrupt:  # noqa: KBI002
            logging.warning("KeyboardInterrupt during lock manager cleanup")
            raise
        except Exception as e:
            logging.error(f"Error clearing locks: {e}")

    # Log cleanup of other subsystems (they don't have explicit shutdown methods)
    logging.debug("Cleaning up subprocess manager...")
    logging.debug("Cleaning up error collector...")
    logging.debug("Firmware ledger persists to disk - no cleanup needed")

    logging.info("✅ Daemon context cleaned up")
