"""
Resource Lock Manager - Unified lock management for daemon operations.

This module provides the ResourceLockManager class which centralizes all
lock management logic that was previously scattered across daemon.py.
It provides context managers for automatic lock acquisition/release and
includes cleanup for unused locks to prevent memory leaks.
"""

import logging
import threading
import time
from contextlib import contextmanager
from dataclasses import dataclass, field
from typing import Iterator


@dataclass
class LockInfo:
    """Information about a lock for debugging and cleanup.

    Attributes:
        lock: The actual threading.Lock object
        created_at: Unix timestamp when lock was created
        last_acquired_at: Unix timestamp when lock was last acquired
        acquisition_count: Number of times lock has been acquired
    """

    lock: threading.Lock
    created_at: float = field(default_factory=time.time)
    last_acquired_at: float | None = None
    acquisition_count: int = 0


class ResourceLockManager:
    """Manages per-port and per-project locks with automatic cleanup.

    This class provides a unified interface for managing locks that protect
    shared resources (serial ports and project directories). It uses context
    managers to ensure locks are always properly released and includes
    periodic cleanup to prevent memory leaks from abandoned locks.

    Example:
        >>> manager = ResourceLockManager()
        >>>
        >>> # Acquire port lock for serial operations
        >>> with manager.acquire_port_lock("COM3"):
        ...     # Perform serial operation
        ...     upload_firmware_to_port("COM3")
        >>>
        >>> # Acquire project lock for build operations
        >>> with manager.acquire_project_lock("/path/to/project"):
        ...     # Perform build operation
        ...     compile_project("/path/to/project")
        >>>
        >>> # Cleanup old unused locks
        >>> manager.cleanup_unused_locks(older_than=3600)
    """

    def __init__(self) -> None:
        """Initialize the ResourceLockManager."""
        self._master_lock = threading.Lock()  # Protects the lock dictionaries
        self._port_locks: dict[str, LockInfo] = {}  # Per-port locks
        self._project_locks: dict[str, LockInfo] = {}  # Per-project locks

    @contextmanager
    def acquire_port_lock(self, port: str, blocking: bool = True) -> Iterator[None]:
        """Acquire a lock for a specific serial port.

        This ensures that only one operation can use a serial port at a time,
        preventing conflicts between deploy and monitor operations.

        Args:
            port: Serial port identifier (e.g., "COM3", "/dev/ttyUSB0")
            blocking: If True, wait for lock. If False, raise RuntimeError if unavailable.

        Yields:
            None (the lock is held for the duration of the context)

        Raises:
            RuntimeError: If blocking=False and lock is not available

        Example:
            >>> manager = ResourceLockManager()
            >>> with manager.acquire_port_lock("COM3"):
            ...     # Only one thread can be here at a time for COM3
            ...     deploy_to_port("COM3")
        """
        lock_info = self._get_or_create_port_lock(port)
        logging.debug(f"Acquiring port lock for: {port} (blocking={blocking})")

        acquired = lock_info.lock.acquire(blocking=blocking)
        if not acquired:
            raise RuntimeError(f"Port lock unavailable for: {port}")

        try:
            lock_info.last_acquired_at = time.time()
            lock_info.acquisition_count += 1
            logging.debug(f"Port lock acquired for: {port} (count={lock_info.acquisition_count})")
            yield
        finally:
            lock_info.lock.release()

    @contextmanager
    def acquire_project_lock(self, project_dir: str, blocking: bool = True) -> Iterator[None]:
        """Acquire a lock for a specific project directory.

        This ensures that only one build operation can run for a project at a time,
        preventing file conflicts and race conditions during compilation.

        Args:
            project_dir: Absolute path to project directory
            blocking: If True, wait for lock. If False, raise RuntimeError if unavailable.

        Yields:
            None (the lock is held for the duration of the context)

        Raises:
            RuntimeError: If blocking=False and lock is not available

        Example:
            >>> manager = ResourceLockManager()
            >>> with manager.acquire_project_lock("/home/user/my_project"):
            ...     # Only one thread can build this project at a time
            ...     build_project("/home/user/my_project")
        """
        lock_info = self._get_or_create_project_lock(project_dir)
        logging.debug(f"Acquiring project lock for: {project_dir} (blocking={blocking})")

        acquired = lock_info.lock.acquire(blocking=blocking)
        if not acquired:
            raise RuntimeError(f"Project lock unavailable for: {project_dir}")

        try:
            lock_info.last_acquired_at = time.time()
            lock_info.acquisition_count += 1
            logging.debug(f"Project lock acquired for: {project_dir} (count={lock_info.acquisition_count})")
            yield
        finally:
            lock_info.lock.release()

    def _get_or_create_port_lock(self, port: str) -> LockInfo:
        """Get or create a lock for the given port.

        Thread-safe: Uses master lock to protect dictionary access.

        Args:
            port: Serial port identifier

        Returns:
            LockInfo for the port
        """
        with self._master_lock:
            if port not in self._port_locks:
                self._port_locks[port] = LockInfo(lock=threading.Lock())
            return self._port_locks[port]

    def _get_or_create_project_lock(self, project_dir: str) -> LockInfo:
        """Get or create a lock for the given project directory.

        Thread-safe: Uses master lock to protect dictionary access.

        Args:
            project_dir: Project directory path

        Returns:
            LockInfo for the project
        """
        with self._master_lock:
            if project_dir not in self._project_locks:
                self._project_locks[project_dir] = LockInfo(lock=threading.Lock())
            return self._project_locks[project_dir]

    def cleanup_unused_locks(self, older_than: float = 3600) -> int:
        """Clean up locks that haven't been acquired recently.

        This prevents memory leaks from locks that were created for operations
        that are no longer running. A lock is considered unused if it hasn't
        been acquired in the specified time period.

        Args:
            older_than: Time in seconds. Locks not acquired in this period are removed.
                       Default is 3600 seconds (1 hour).

        Returns:
            Number of locks removed

        Example:
            >>> manager = ResourceLockManager()
            >>> # Remove locks not used in the last hour
            >>> removed = manager.cleanup_unused_locks(older_than=3600)
            >>> print(f"Cleaned up {removed} unused locks")
        """
        current_time = time.time()
        removed_count = 0

        with self._master_lock:
            # Clean up port locks
            ports_to_remove = []
            for port, lock_info in self._port_locks.items():
                if lock_info.last_acquired_at is None:
                    # Lock was created but never acquired - remove if old enough
                    if current_time - lock_info.created_at > older_than:
                        ports_to_remove.append(port)
                elif current_time - lock_info.last_acquired_at > older_than:
                    # Lock hasn't been acquired recently
                    ports_to_remove.append(port)

            for port in ports_to_remove:
                del self._port_locks[port]
                removed_count += 1

            # Clean up project locks
            projects_to_remove = []
            for project_dir, lock_info in self._project_locks.items():
                if lock_info.last_acquired_at is None:
                    # Lock was created but never acquired - remove if old enough
                    if current_time - lock_info.created_at > older_than:
                        projects_to_remove.append(project_dir)
                elif current_time - lock_info.last_acquired_at > older_than:
                    # Lock hasn't been acquired recently
                    projects_to_remove.append(project_dir)

            for project_dir in projects_to_remove:
                del self._project_locks[project_dir]
                removed_count += 1

        if removed_count > 0:
            logging.info(f"Cleaned up {removed_count} unused locks")

        return removed_count

    def get_lock_status(self) -> dict[str, dict[str, int]]:
        """Get current lock status for debugging.

        Returns a snapshot of all locks and their acquisition counts.

        Returns:
            Dictionary with 'port_locks' and 'project_locks' keys, each containing
            a mapping of resource identifier to acquisition count.

        Example:
            >>> manager = ResourceLockManager()
            >>> status = manager.get_lock_status()
            >>> print(f"Port locks: {status['port_locks']}")
            >>> print(f"Project locks: {status['project_locks']}")
        """
        with self._master_lock:
            return {
                "port_locks": {port: info.acquisition_count for port, info in self._port_locks.items()},
                "project_locks": {project: info.acquisition_count for project, info in self._project_locks.items()},
            }

    def get_lock_count(self) -> dict[str, int]:
        """Get the total number of locks currently held.

        Returns:
            Dictionary with 'port_locks' and 'project_locks' counts.

        Example:
            >>> manager = ResourceLockManager()
            >>> counts = manager.get_lock_count()
            >>> print(f"Total port locks: {counts['port_locks']}")
            >>> print(f"Total project locks: {counts['project_locks']}")
        """
        with self._master_lock:
            return {
                "port_locks": len(self._port_locks),
                "project_locks": len(self._project_locks),
            }
