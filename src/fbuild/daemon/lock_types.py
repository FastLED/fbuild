"""
Lock Status Types - Structured types for lock status reporting.

This module provides dataclasses for representing lock status information
in a type-safe manner, replacing complex nested dict types.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    pass


@dataclass
class ResourceLock:
    """A lock on a specific resource (port or project).

    Attributes:
        resource_id: Port name or project path
        lock_info: Information about the lock
    """

    resource_id: str
    lock_info: Any  # LockInfo - using Any to avoid circular import

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization.

        Returns:
            Dictionary with resource_id and lock_info fields
        """
        return {
            "resource_id": self.resource_id,
            "lock_info": self.lock_info.to_dict(),
        }

    @classmethod
    def from_tuple(cls, data: tuple[str, Any]) -> ResourceLock:
        """Create ResourceLock from tuple.

        Args:
            data: Tuple of (resource_id, lock_info)

        Returns:
            ResourceLock instance
        """
        return cls(resource_id=data[0], lock_info=data[1])


@dataclass
class LockStatusSummary:
    """Summary of lock status across all resources.

    Attributes:
        port_locks: Dictionary mapping port names to lock info (LockInfo objects)
        project_locks: Dictionary mapping project paths to lock info (LockInfo objects)
    """

    port_locks: dict[str, Any] = field(default_factory=dict)  # dict[str, LockInfo]
    project_locks: dict[str, Any] = field(default_factory=dict)  # dict[str, LockInfo]

    def get_port_lock(self, port: str) -> Any | None:  # LockInfo | None
        """Get lock info for a specific port.

        Args:
            port: Port identifier

        Returns:
            LockInfo for the port, or None if not locked
        """
        return self.port_locks.get(port)

    def get_project_lock(self, project: str) -> Any | None:  # LockInfo | None
        """Get lock info for a specific project.

        Args:
            project: Project path

        Returns:
            LockInfo for the project, or None if not locked
        """
        return self.project_locks.get(project)

    def total_locks(self) -> int:
        """Get total number of locks.

        Returns:
            Sum of port and project locks
        """
        return len(self.port_locks) + len(self.project_locks)

    def to_dict(self) -> dict[str, dict[str, dict[str, Any]]]:
        """Convert to dictionary for JSON serialization.

        Returns:
            Dictionary with port_locks and project_locks keys
        """
        return {
            "port_locks": {port: info.to_dict() for port, info in self.port_locks.items()},
            "project_locks": {project: info.to_dict() for project, info in self.project_locks.items()},
        }

    @classmethod
    def from_dict(cls, data: dict[str, dict[str, dict[str, Any]]]) -> "LockStatusSummary":
        """Create LockStatusSummary from dictionary.

        Args:
            data: Dictionary with port_locks and project_locks keys

        Returns:
            LockStatusSummary instance

        Note:
            This is a placeholder - full deserialization of LockInfo requires
            reconstructing threading.Lock objects which isn't feasible.
            Use this only for testing or when lock objects aren't needed.
        """
        return cls(port_locks={}, project_locks={})


@dataclass
class StaleLocksSummary:
    """Summary of stale locks that need cleanup.

    Attributes:
        stale_port_locks: List of stale port locks
        stale_project_locks: List of stale project locks
    """

    stale_port_locks: list[ResourceLock] = field(default_factory=list)
    stale_project_locks: list[ResourceLock] = field(default_factory=list)

    def total_stale(self) -> int:
        """Get total number of stale locks.

        Returns:
            Sum of stale port and project locks
        """
        return len(self.stale_port_locks) + len(self.stale_project_locks)

    def has_stale_locks(self) -> bool:
        """Check if there are any stale locks.

        Returns:
            True if any stale locks exist
        """
        return self.total_stale() > 0

    def to_dict(self) -> dict[str, list[dict[str, Any]]]:
        """Convert to dictionary for JSON serialization.

        Returns:
            Dictionary with port_locks and project_locks keys
        """
        return {
            "port_locks": [lock.to_dict() for lock in self.stale_port_locks],
            "project_locks": [lock.to_dict() for lock in self.stale_project_locks],
        }


@dataclass
class HeldLocksSummary:
    """Summary of currently held locks.

    Attributes:
        held_port_locks: List of held port locks
        held_project_locks: List of held project locks
    """

    held_port_locks: list[ResourceLock] = field(default_factory=list)
    held_project_locks: list[ResourceLock] = field(default_factory=list)

    def total_held(self) -> int:
        """Get total number of held locks.

        Returns:
            Sum of held port and project locks
        """
        return len(self.held_port_locks) + len(self.held_project_locks)

    def has_held_locks(self) -> bool:
        """Check if there are any held locks.

        Returns:
            True if any held locks exist
        """
        return self.total_held() > 0

    def get_held_port_lock(self, port: str) -> ResourceLock | None:
        """Get held lock for a specific port.

        Args:
            port: Port identifier

        Returns:
            ResourceLock for the port, or None if not held
        """
        return next((lock for lock in self.held_port_locks if lock.resource_id == port), None)

    def get_held_project_lock(self, project: str) -> ResourceLock | None:
        """Get held lock for a specific project.

        Args:
            project: Project path

        Returns:
            ResourceLock for the project, or None if not held
        """
        return next((lock for lock in self.held_project_locks if lock.resource_id == project), None)

    def to_dict(self) -> dict[str, list[dict[str, Any]]]:
        """Convert to dictionary for JSON serialization.

        Returns:
            Dictionary with port_locks and project_locks keys
        """
        return {
            "port_locks": [lock.to_dict() for lock in self.held_port_locks],
            "project_locks": [lock.to_dict() for lock in self.held_project_locks],
        }
