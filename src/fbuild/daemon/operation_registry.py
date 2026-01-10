"""
Operation Registry - Structured operation state tracking.

This module provides a registry for tracking all daemon operations (build/deploy/monitor)
with structured state management, replacing the simple boolean _operation_in_progress flag.
"""

import logging
import threading
import time
from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Optional

from fbuild.daemon.messages import OperationType


class OperationState(Enum):
    """State of a daemon operation."""

    QUEUED = "queued"
    RUNNING = "running"
    COMPLETED = "completed"
    FAILED = "failed"
    CANCELLED = "cancelled"


@dataclass
class Operation:
    """Tracks a daemon operation (build/deploy/monitor)."""

    operation_id: str
    operation_type: OperationType
    project_dir: str
    environment: str
    state: OperationState
    request_id: str
    caller_pid: int
    created_at: float = field(default_factory=time.time)
    started_at: Optional[float] = None
    completed_at: Optional[float] = None
    error_message: Optional[str] = None
    result: Optional[Any] = None

    # Subprocess tracking
    subprocess_ids: list[str] = field(default_factory=list)
    compilation_job_ids: list[str] = field(default_factory=list)

    def duration(self) -> Optional[float]:
        """Get operation duration in seconds.

        Returns:
            Duration in seconds, or None if not complete
        """
        if self.started_at and self.completed_at:
            return self.completed_at - self.started_at
        return None

    def elapsed_time(self) -> Optional[float]:
        """Get elapsed time since operation started.

        Returns:
            Elapsed time in seconds, or None if not started
        """
        if self.started_at:
            return time.time() - self.started_at
        return None


class OperationRegistry:
    """Registry for tracking all daemon operations."""

    def __init__(self, max_history: int = 100):
        """Initialize operation registry.

        Args:
            max_history: Maximum number of completed operations to retain
        """
        self.operations: dict[str, Operation] = {}
        self.lock = threading.Lock()
        self.max_history = max_history

        logging.info(f"OperationRegistry initialized (max_history={max_history})")

    def register_operation(self, operation: Operation) -> str:
        """Register new operation.

        Args:
            operation: Operation to register

        Returns:
            Operation ID
        """
        with self.lock:
            self.operations[operation.operation_id] = operation
            self._cleanup_old_operations()

        logging.info(f"Registered operation {operation.operation_id}: {operation.operation_type.value} {operation.project_dir}")
        return operation.operation_id

    def get_operation(self, operation_id: str) -> Optional[Operation]:
        """Get operation by ID.

        Args:
            operation_id: Operation ID to query

        Returns:
            Operation or None if not found
        """
        with self.lock:
            return self.operations.get(operation_id)

    def update_state(self, operation_id: str, state: OperationState, **kwargs: Any) -> None:
        """Update operation state.

        Args:
            operation_id: Operation ID to update
            state: New state
            **kwargs: Additional fields to update
        """
        with self.lock:
            if operation_id not in self.operations:
                logging.warning(f"Cannot update unknown operation: {operation_id}")
                return

            op = self.operations[operation_id]
            old_state = op.state
            op.state = state

            # Auto-update timestamps
            if state == OperationState.RUNNING and op.started_at is None:
                op.started_at = time.time()
            elif state in (OperationState.COMPLETED, OperationState.FAILED, OperationState.CANCELLED):
                if op.completed_at is None:
                    op.completed_at = time.time()

            # Update additional fields
            for key, value in kwargs.items():
                if hasattr(op, key):
                    setattr(op, key, value)

            logging.debug(f"Operation {operation_id} state: {old_state.value} -> {state.value}")

    def get_active_operations(self) -> list[Operation]:
        """Get all active (running/queued) operations.

        Returns:
            List of active operations
        """
        with self.lock:
            return [op for op in self.operations.values() if op.state in (OperationState.QUEUED, OperationState.RUNNING)]

    def get_operations_by_project(self, project_dir: str) -> list[Operation]:
        """Get all operations for a specific project.

        Args:
            project_dir: Project directory path

        Returns:
            List of operations for the project
        """
        with self.lock:
            return [op for op in self.operations.values() if op.project_dir == project_dir]

    def is_project_busy(self, project_dir: str) -> bool:
        """Check if a project has any active operations.

        Args:
            project_dir: Project directory path

        Returns:
            True if project has active operations
        """
        with self.lock:
            return any(op.project_dir == project_dir and op.state in (OperationState.QUEUED, OperationState.RUNNING) for op in self.operations.values())

    def get_statistics(self) -> dict[str, int]:
        """Get operation statistics.

        Returns:
            Dictionary with operation counts by state
        """
        with self.lock:
            stats = {
                "total_operations": len(self.operations),
                "queued": sum(1 for op in self.operations.values() if op.state == OperationState.QUEUED),
                "running": sum(1 for op in self.operations.values() if op.state == OperationState.RUNNING),
                "completed": sum(1 for op in self.operations.values() if op.state == OperationState.COMPLETED),
                "failed": sum(1 for op in self.operations.values() if op.state == OperationState.FAILED),
                "cancelled": sum(1 for op in self.operations.values() if op.state == OperationState.CANCELLED),
            }
        return stats

    def _cleanup_old_operations(self) -> None:
        """Remove old completed operations beyond max_history."""
        completed_ops = sorted(
            [op for op in self.operations.values() if op.state in (OperationState.COMPLETED, OperationState.FAILED, OperationState.CANCELLED)],
            key=lambda x: x.completed_at or 0,
        )

        if len(completed_ops) > self.max_history:
            to_remove = completed_ops[: len(completed_ops) - self.max_history]
            for op in to_remove:
                del self.operations[op.operation_id]

            logging.debug(f"Cleaned up {len(to_remove)} old operations")

    def clear_completed_operations(self, older_than_seconds: Optional[float] = None) -> int:
        """Clear completed operations.

        Args:
            older_than_seconds: Only clear operations older than this (None = all)

        Returns:
            Number of operations cleared
        """
        with self.lock:
            now = time.time()
            to_remove = []

            for op_id, op in self.operations.items():
                if op.state not in (
                    OperationState.COMPLETED,
                    OperationState.FAILED,
                    OperationState.CANCELLED,
                ):
                    continue

                if older_than_seconds is None:
                    to_remove.append(op_id)
                elif op.completed_at and (now - op.completed_at) > older_than_seconds:
                    to_remove.append(op_id)

            for op_id in to_remove:
                del self.operations[op_id]

            if to_remove:
                logging.info(f"Cleared {len(to_remove)} completed operations")

            return len(to_remove)
