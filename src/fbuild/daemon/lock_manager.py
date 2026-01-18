"""
Resource Lock Manager - Unified lock management for daemon operations.

This module provides the ResourceLockManager class which centralizes all
lock management logic. Key features:
- Per-port and per-project locks with context managers
- Lock timeout/expiry for automatic stale lock detection
- Lock holder tracking for better error messages
- Force-release capability for stuck locks
- Automatic cleanup of stale locks
"""

import logging
import threading
import time
from contextlib import contextmanager
from dataclasses import dataclass, field
from typing import Any, Iterator

# Default lock timeout: 30 minutes (for long builds)
DEFAULT_LOCK_TIMEOUT = 1800.0

# Stale lock threshold: locks older than this with no activity are candidates for cleanup
STALE_LOCK_THRESHOLD = 3600.0  # 1 hour


@dataclass
class LockInfo:
    """Information about a lock for debugging, timeout detection, and cleanup.

    Attributes:
        lock: The actual threading.Lock object
        created_at: Unix timestamp when lock was created
        acquired_at: Unix timestamp when lock was last acquired (None if not held)
        last_released_at: Unix timestamp when lock was last released
        acquisition_count: Number of times lock has been acquired
        holder_thread_id: Thread ID currently holding the lock (None if not held)
        holder_operation_id: Operation ID currently holding the lock
        holder_description: Human-readable description of what's holding the lock
        timeout: Maximum time in seconds the lock can be held before considered stale
    """

    lock: threading.Lock
    created_at: float = field(default_factory=time.time)
    acquired_at: float | None = None
    last_released_at: float | None = None
    acquisition_count: int = 0
    holder_thread_id: int | None = None
    holder_operation_id: str | None = None
    holder_description: str | None = None
    timeout: float = DEFAULT_LOCK_TIMEOUT

    def is_held(self) -> bool:
        """Check if lock is currently held."""
        return self.acquired_at is not None and self.last_released_at is None or (self.acquired_at is not None and self.last_released_at is not None and self.acquired_at > self.last_released_at)

    def is_stale(self) -> bool:
        """Check if lock is stale (held beyond timeout)."""
        if not self.is_held():
            return False
        if self.acquired_at is None:
            return False
        hold_time = time.time() - self.acquired_at
        return hold_time > self.timeout

    def hold_duration(self) -> float | None:
        """Get how long the lock has been held."""
        if not self.is_held() or self.acquired_at is None:
            return None
        return time.time() - self.acquired_at

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return {
            "created_at": self.created_at,
            "acquired_at": self.acquired_at,
            "last_released_at": self.last_released_at,
            "acquisition_count": self.acquisition_count,
            "holder_thread_id": self.holder_thread_id,
            "holder_operation_id": self.holder_operation_id,
            "holder_description": self.holder_description,
            "timeout": self.timeout,
            "is_held": self.is_held(),
            "is_stale": self.is_stale(),
            "hold_duration": self.hold_duration(),
        }


class LockAcquisitionError(RuntimeError):
    """Error raised when a lock cannot be acquired.

    Provides detailed information about what's holding the lock.
    """

    def __init__(
        self,
        resource_type: str,
        resource_id: str,
        lock_info: LockInfo | None = None,
    ):
        self.resource_type = resource_type
        self.resource_id = resource_id
        self.lock_info = lock_info

        # Build detailed error message
        if lock_info is not None and lock_info.is_held():
            holder_desc = lock_info.holder_description or "unknown operation"
            hold_duration = lock_info.hold_duration()
            duration_str = f" (held for {hold_duration:.1f}s)" if hold_duration else ""
            if lock_info.is_stale():
                message = (
                    f"{resource_type.capitalize()} lock unavailable for: {resource_id}. " + f"STALE lock held by: {holder_desc}{duration_str}. " + "Consider force-releasing with clear_stale_locks()."
                )
            else:
                message = f"{resource_type.capitalize()} lock unavailable for: {resource_id}. " + f"Currently held by: {holder_desc}{duration_str}."
        else:
            message = f"{resource_type.capitalize()} lock unavailable for: {resource_id}"

        super().__init__(message)


class ResourceLockManager:
    """Manages per-port and per-project locks with timeout detection and cleanup.

    This class provides a unified interface for managing locks that protect
    shared resources (serial ports and project directories). Features:
    - Context managers for automatic lock acquisition/release
    - Lock timeout detection for stale lock cleanup
    - Lock holder tracking for informative error messages
    - Force-release capability for stuck locks
    - Thread-safe operations

    Example:
        >>> manager = ResourceLockManager()
        >>>
        >>> # Acquire port lock for serial operations
        >>> with manager.acquire_port_lock("COM3", operation_id="deploy_123",
        ...                                description="Deploy to ESP32"):
        ...     upload_firmware_to_port("COM3")
        >>>
        >>> # Check for stale locks
        >>> stale = manager.get_stale_locks()
        >>> if stale:
        ...     print(f"Found {len(stale)} stale locks")
        ...     manager.force_release_stale_locks()
    """

    def __init__(self) -> None:
        """Initialize the ResourceLockManager."""
        self._master_lock = threading.Lock()  # Protects the lock dictionaries
        self._port_locks: dict[str, LockInfo] = {}  # Per-port locks
        self._project_locks: dict[str, LockInfo] = {}  # Per-project locks

    @contextmanager
    def acquire_port_lock(
        self,
        port: str,
        blocking: bool = True,
        timeout: float = DEFAULT_LOCK_TIMEOUT,
        operation_id: str | None = None,
        description: str | None = None,
    ) -> Iterator[None]:
        """Acquire a lock for a specific serial port.

        This ensures that only one operation can use a serial port at a time,
        preventing conflicts between deploy and monitor operations.

        Args:
            port: Serial port identifier (e.g., "COM3", "/dev/ttyUSB0")
            blocking: If True, wait for lock. If False, raise LockAcquisitionError if unavailable.
            timeout: Maximum time the lock can be held before considered stale.
            operation_id: Identifier for the operation holding the lock.
            description: Human-readable description of what's holding the lock.

        Yields:
            None (the lock is held for the duration of the context)

        Raises:
            LockAcquisitionError: If blocking=False and lock is not available
        """
        lock_info = self._get_or_create_port_lock(port, timeout)
        logging.debug(f"Acquiring port lock for: {port} (blocking={blocking})")

        acquired = lock_info.lock.acquire(blocking=blocking)
        if not acquired:
            raise LockAcquisitionError("port", port, lock_info)

        try:
            # Record lock acquisition details
            with self._master_lock:
                lock_info.acquired_at = time.time()
                lock_info.acquisition_count += 1
                lock_info.holder_thread_id = threading.get_ident()
                lock_info.holder_operation_id = operation_id
                lock_info.holder_description = description or f"Operation on port {port}"
                lock_info.timeout = timeout

            logging.debug(f"Port lock acquired for: {port} " + f"(count={lock_info.acquisition_count}, operation={operation_id})")
            yield
        finally:
            # Clear holder info before releasing
            with self._master_lock:
                lock_info.last_released_at = time.time()
                lock_info.holder_thread_id = None
                lock_info.holder_operation_id = None
                lock_info.holder_description = None
            lock_info.lock.release()
            logging.debug(f"Port lock released for: {port}")

    @contextmanager
    def acquire_project_lock(
        self,
        project_dir: str,
        blocking: bool = True,
        timeout: float = DEFAULT_LOCK_TIMEOUT,
        operation_id: str | None = None,
        description: str | None = None,
    ) -> Iterator[None]:
        """Acquire a lock for a specific project directory.

        This ensures that only one build operation can run for a project at a time,
        preventing file conflicts and race conditions during compilation.

        Args:
            project_dir: Absolute path to project directory
            blocking: If True, wait for lock. If False, raise LockAcquisitionError if unavailable.
            timeout: Maximum time the lock can be held before considered stale.
            operation_id: Identifier for the operation holding the lock.
            description: Human-readable description of what's holding the lock.

        Yields:
            None (the lock is held for the duration of the context)

        Raises:
            LockAcquisitionError: If blocking=False and lock is not available
        """
        lock_info = self._get_or_create_project_lock(project_dir, timeout)
        logging.debug(f"Acquiring project lock for: {project_dir} (blocking={blocking})")

        acquired = lock_info.lock.acquire(blocking=blocking)
        if not acquired:
            raise LockAcquisitionError("project", project_dir, lock_info)

        try:
            # Record lock acquisition details
            with self._master_lock:
                lock_info.acquired_at = time.time()
                lock_info.acquisition_count += 1
                lock_info.holder_thread_id = threading.get_ident()
                lock_info.holder_operation_id = operation_id
                lock_info.holder_description = description or f"Build for {project_dir}"
                lock_info.timeout = timeout

            logging.debug(f"Project lock acquired for: {project_dir} " + f"(count={lock_info.acquisition_count}, operation={operation_id})")
            yield
        finally:
            # Clear holder info before releasing
            with self._master_lock:
                lock_info.last_released_at = time.time()
                lock_info.holder_thread_id = None
                lock_info.holder_operation_id = None
                lock_info.holder_description = None
            lock_info.lock.release()
            logging.debug(f"Project lock released for: {project_dir}")

    def _get_or_create_port_lock(self, port: str, timeout: float = DEFAULT_LOCK_TIMEOUT) -> LockInfo:
        """Get or create a lock for the given port."""
        with self._master_lock:
            if port not in self._port_locks:
                self._port_locks[port] = LockInfo(lock=threading.Lock(), timeout=timeout)
            return self._port_locks[port]

    def _get_or_create_project_lock(self, project_dir: str, timeout: float = DEFAULT_LOCK_TIMEOUT) -> LockInfo:
        """Get or create a lock for the given project directory."""
        with self._master_lock:
            if project_dir not in self._project_locks:
                self._project_locks[project_dir] = LockInfo(lock=threading.Lock(), timeout=timeout)
            return self._project_locks[project_dir]

    def get_stale_locks(self) -> dict[str, list[tuple[str, LockInfo]]]:
        """Get all locks that are stale (held beyond their timeout).

        Returns:
            Dictionary with 'port_locks' and 'project_locks' keys, each containing
            a list of (resource_id, lock_info) tuples for stale locks.
        """
        with self._master_lock:
            stale_ports = [(port, info) for port, info in self._port_locks.items() if info.is_stale()]
            stale_projects = [(project, info) for project, info in self._project_locks.items() if info.is_stale()]
            return {
                "port_locks": stale_ports,
                "project_locks": stale_projects,
            }

    def get_held_locks(self) -> dict[str, list[tuple[str, LockInfo]]]:
        """Get all locks that are currently held.

        Returns:
            Dictionary with 'port_locks' and 'project_locks' keys, each containing
            a list of (resource_id, lock_info) tuples for held locks.
        """
        with self._master_lock:
            held_ports = [(port, info) for port, info in self._port_locks.items() if info.is_held()]
            held_projects = [(project, info) for project, info in self._project_locks.items() if info.is_held()]
            return {
                "port_locks": held_ports,
                "project_locks": held_projects,
            }

    def force_release_lock(self, resource_type: str, resource_id: str) -> bool:
        """Force-release a lock (use with caution - may cause race conditions).

        This should only be used to clear stale locks from stuck operations.
        Force-releasing an active lock may cause data corruption.

        Args:
            resource_type: "port" or "project"
            resource_id: The port or project directory identifier

        Returns:
            True if lock was force-released, False if lock not found
        """
        with self._master_lock:
            if resource_type == "port":
                locks_dict = self._port_locks
            elif resource_type == "project":
                locks_dict = self._project_locks
            else:
                logging.error(f"Unknown resource type: {resource_type}")
                return False

            if resource_id not in locks_dict:
                logging.warning(f"Lock not found for {resource_type}: {resource_id}")
                return False

            lock_info = locks_dict[resource_id]
            if not lock_info.is_held():
                logging.info(f"Lock for {resource_type} {resource_id} is not held")
                return False

            # Clear holder info and mark as released
            logging.warning(f"Force-releasing {resource_type} lock for: {resource_id} " + f"(was held by: {lock_info.holder_description})")
            lock_info.last_released_at = time.time()
            lock_info.holder_thread_id = None
            lock_info.holder_operation_id = None
            lock_info.holder_description = None

            # Try to release the lock if it's actually held
            # Note: This may fail if the lock isn't held by this thread
            try:
                lock_info.lock.release()
            except RuntimeError:
                # Lock wasn't held - this is OK for force-release
                pass

            return True

    def force_release_stale_locks(self) -> int:
        """Force-release all stale locks.

        Returns:
            Number of locks force-released
        """
        stale = self.get_stale_locks()
        released = 0

        for port, _ in stale["port_locks"]:
            if self.force_release_lock("port", port):
                released += 1

        for project, _ in stale["project_locks"]:
            if self.force_release_lock("project", project):
                released += 1

        if released > 0:
            logging.info(f"Force-released {released} stale locks")

        return released

    def cleanup_unused_locks(self, older_than: float = STALE_LOCK_THRESHOLD) -> int:
        """Clean up locks that haven't been acquired recently.

        This prevents memory leaks from locks that were created for operations
        that are no longer running. A lock is considered unused if it:
        - Is not currently held AND
        - Hasn't been acquired in the specified time period

        Args:
            older_than: Time in seconds. Locks not acquired in this period are removed.

        Returns:
            Number of locks removed
        """
        current_time = time.time()
        removed_count = 0

        with self._master_lock:
            # Clean up port locks
            ports_to_remove = []
            for port, lock_info in self._port_locks.items():
                if lock_info.is_held():
                    continue  # Don't remove held locks

                # Check last activity time
                last_activity = lock_info.last_released_at or lock_info.created_at
                if current_time - last_activity > older_than:
                    ports_to_remove.append(port)

            for port in ports_to_remove:
                del self._port_locks[port]
                removed_count += 1
                logging.debug(f"Cleaned up unused port lock: {port}")

            # Clean up project locks
            projects_to_remove = []
            for project_dir, lock_info in self._project_locks.items():
                if lock_info.is_held():
                    continue  # Don't remove held locks

                # Check last activity time
                last_activity = lock_info.last_released_at or lock_info.created_at
                if current_time - last_activity > older_than:
                    projects_to_remove.append(project_dir)

            for project_dir in projects_to_remove:
                del self._project_locks[project_dir]
                removed_count += 1
                logging.debug(f"Cleaned up unused project lock: {project_dir}")

        if removed_count > 0:
            logging.info(f"Cleaned up {removed_count} unused locks")

        return removed_count

    def get_lock_status(self) -> dict[str, dict[str, int]]:
        """Get current lock status for debugging.

        Returns:
            Dictionary with 'port_locks' and 'project_locks' keys, each containing
            a mapping of resource identifier to acquisition count.
        """
        with self._master_lock:
            return {
                "port_locks": {port: info.acquisition_count for port, info in self._port_locks.items()},
                "project_locks": {project: info.acquisition_count for project, info in self._project_locks.items()},
            }

    def get_lock_details(self) -> dict[str, dict[str, dict[str, Any]]]:
        """Get detailed lock information for debugging and status reporting.

        Returns:
            Dictionary with 'port_locks' and 'project_locks' keys, each containing
            a mapping of resource identifier to detailed lock info dict.
        """
        with self._master_lock:
            return {
                "port_locks": {port: info.to_dict() for port, info in self._port_locks.items()},
                "project_locks": {project: info.to_dict() for project, info in self._project_locks.items()},
            }

    def get_lock_count(self) -> dict[str, int]:
        """Get the total number of locks currently tracked.

        Returns:
            Dictionary with 'port_locks' and 'project_locks' counts.
        """
        with self._master_lock:
            return {
                "port_locks": len(self._port_locks),
                "project_locks": len(self._project_locks),
            }

    def clear_all_locks(self) -> int:
        """Clear all locks (use with extreme caution - only for daemon restart).

        This force-releases all locks and clears the lock dictionaries.
        Should only be used during daemon shutdown/restart.

        Returns:
            Number of locks cleared
        """
        with self._master_lock:
            count = len(self._port_locks) + len(self._project_locks)

            # Force release any held locks
            for port, lock_info in self._port_locks.items():
                if lock_info.is_held():
                    logging.warning(f"Clearing held port lock: {port}")
                    try:
                        lock_info.lock.release()
                    except RuntimeError:
                        pass

            for project, lock_info in self._project_locks.items():
                if lock_info.is_held():
                    logging.warning(f"Clearing held project lock: {project}")
                    try:
                        lock_info.lock.release()
                    except RuntimeError:
                        pass

            self._port_locks.clear()
            self._project_locks.clear()

            if count > 0:
                logging.info(f"Cleared all {count} locks")

            return count
