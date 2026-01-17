"""
Status Manager - Centralized status file management for daemon operations.

This module provides the StatusManager class which handles all status file
I/O operations with proper locking and atomic writes. It eliminates the
scattered update_status() calls throughout daemon.py and provides a clean
API for status management.
"""

import json
import logging
import threading
import time
from pathlib import Path
from typing import TYPE_CHECKING, Any

from fbuild.daemon.messages import DaemonState, DaemonStatus
from fbuild.daemon.port_state_manager import PortStateManager
from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

if TYPE_CHECKING:
    from fbuild.daemon.lock_manager import ResourceLockManager


class StatusManager:
    """Manages daemon status file operations.

    This class provides centralized management of the daemon status file,
    ensuring:
    - Atomic writes (write to temp file + rename)
    - Thread-safe operations (internal locking)
    - Consistent status structure
    - Request ID validation

    The status file is used for communication between the daemon and client,
    allowing the client to monitor the progress of operations.

    Example:
        >>> manager = StatusManager(status_file_path, daemon_pid=1234)
        >>> manager.update_status(
        ...     DaemonState.BUILDING,
        ...     "Building firmware",
        ...     environment="esp32dev",
        ...     project_dir="/path/to/project"
        ... )
        >>> status = manager.read_status()
        >>> print(status.state)
        DaemonState.BUILDING
    """

    def __init__(
        self,
        status_file: Path,
        daemon_pid: int,
        daemon_started_at: float | None = None,
        port_state_manager: PortStateManager | None = None,
        lock_manager: "ResourceLockManager | None" = None,
    ):
        """Initialize the StatusManager.

        Args:
            status_file: Path to the status file
            daemon_pid: PID of the daemon process
            daemon_started_at: Timestamp when daemon started (defaults to now)
            port_state_manager: Optional PortStateManager for including port state in status
            lock_manager: Optional ResourceLockManager for including lock state in status
        """
        self.status_file = status_file
        self.daemon_pid = daemon_pid
        self.daemon_started_at = daemon_started_at if daemon_started_at is not None else time.time()
        self._lock = threading.Lock()
        self._operation_in_progress = False
        self._port_state_manager = port_state_manager
        self._lock_manager = lock_manager

        # Ensure parent directory exists
        self.status_file.parent.mkdir(parents=True, exist_ok=True)

    def update_status(
        self,
        state: DaemonState,
        message: str,
        operation_in_progress: bool | None = None,
        **kwargs: Any,
    ) -> None:
        """Update the status file with current daemon state.

        This method is thread-safe and performs atomic writes to prevent
        corruption during concurrent access.

        Args:
            state: DaemonState enum value
            message: Human-readable status message
            operation_in_progress: Whether an operation is in progress (None = use current value)
            **kwargs: Additional fields to include in status (e.g., environment, project_dir)

        Example:
            >>> manager.update_status(
            ...     DaemonState.BUILDING,
            ...     "Building firmware",
            ...     environment="esp32dev",
            ...     project_dir="/path/to/project",
            ...     request_id="build_1234567890",
            ... )
        """
        with self._lock:

            # Update internal operation state if provided
            if operation_in_progress is not None:
                self._operation_in_progress = operation_in_progress

            # Get port state summary if port_state_manager is available
            ports_summary: dict[str, Any] = {}
            if self._port_state_manager is not None:
                ports_summary = self._port_state_manager.get_ports_summary()

            # Get lock state summary if lock_manager is available
            locks_summary: dict[str, Any] = {}
            if self._lock_manager is not None:
                locks_summary = self._lock_manager.get_lock_details()

            # Create typed DaemonStatus object
            status_obj = DaemonStatus(
                state=state,
                message=message,
                updated_at=time.time(),
                daemon_pid=self.daemon_pid,
                daemon_started_at=self.daemon_started_at,
                operation_in_progress=self._operation_in_progress,
                ports=ports_summary,
                locks=locks_summary,
                **kwargs,
            )

            logging.debug(f"Writing status to file (additional fields: {len(kwargs)})")
            self._write_status_atomic(status_obj.to_dict())

    def read_status(self) -> DaemonStatus:
        """Read and parse the status file.

        Returns:
            DaemonStatus object with current daemon state

        If the file doesn't exist or is corrupted, returns a default status
        indicating the daemon is idle.
        """
        with self._lock:
            if not self.status_file.exists():
                return self._get_default_status()

            try:
                with open(self.status_file, encoding="utf-8") as f:
                    data = json.load(f)

                status = DaemonStatus.from_dict(data)
                return status

            except KeyboardInterrupt as ke:
                handle_keyboard_interrupt_properly(ke)
            except (json.JSONDecodeError, ValueError) as e:
                logging.warning(f"Corrupted status file detected: {e}")
                logging.warning("Creating fresh status file")

                # Write fresh status file
                default_status = self._get_default_status()
                self._write_status_atomic(default_status.to_dict())
                return default_status

            except Exception as e:
                logging.error(f"Unexpected error reading status file: {e}")
                default_status = self._get_default_status()
                self._write_status_atomic(default_status.to_dict())
                return default_status

    def set_operation_in_progress(self, in_progress: bool) -> None:
        """Set the operation_in_progress flag.

        This is used to track whether the daemon is currently executing
        an operation. It's typically set to True when starting an operation
        and False when completing or failing.

        Args:
            in_progress: Whether an operation is in progress
        """
        with self._lock:
            self._operation_in_progress = in_progress

    def get_operation_in_progress(self) -> bool:
        """Get the current operation_in_progress flag.

        Returns:
            True if an operation is in progress, False otherwise
        """
        with self._lock:
            return self._operation_in_progress

    def _write_status_atomic(self, status: dict[str, Any]) -> None:
        """Write status file atomically to prevent corruption during writes.

        This method writes to a temporary file first, then atomically renames
        it to the actual status file. This ensures the status file is never
        in a partially-written state.

        Args:
            status: Status dictionary to write
        """
        temp_file = self.status_file.with_suffix(".tmp")
        logging.debug(f"Using temp file: {temp_file}")

        try:
            logging.debug(f"Writing JSON to temp file ({len(status)} keys)...")
            with open(temp_file, "w", encoding="utf-8") as f:
                json.dump(status, f, indent=2)
            # Atomic rename
            temp_file.replace(self.status_file)

        except KeyboardInterrupt:  # noqa: KBI002
            logging.warning("KeyboardInterrupt during status file write, cleaning up temp file")
            temp_file.unlink(missing_ok=True)
            raise
        except Exception as e:
            logging.error(f"Failed to write status file: {e}")
            temp_file.unlink(missing_ok=True)

    def _get_default_status(self) -> DaemonStatus:
        """Get default idle status.

        Returns:
            DaemonStatus object indicating daemon is idle
        """
        return DaemonStatus(
            state=DaemonState.IDLE,
            message="Daemon is idle",
            updated_at=time.time(),
            daemon_pid=self.daemon_pid,
            daemon_started_at=self.daemon_started_at,
            operation_in_progress=False,
        )
