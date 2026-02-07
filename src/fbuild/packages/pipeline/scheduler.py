"""DAG-based dependency scheduler for the parallel package pipeline.

Resolves package dependencies and emits tasks in topological order,
ensuring that a task only becomes ready when all its dependencies
have completed successfully.
"""

import threading
from typing import Any

from .models import PackageTask, TaskPhase


class CyclicDependencyError(ValueError):
    """Raised when the dependency graph contains a cycle."""

    pass


class DependencyScheduler:
    """Schedules package tasks based on their dependency DAG.

    Thread-safe: multiple pool threads can call mark_phase() concurrently
    while the main loop calls get_ready_tasks().

    Usage:
        scheduler = DependencyScheduler()
        scheduler.add_task(task_a)
        scheduler.add_task(task_b)
        scheduler.validate()  # raises CyclicDependencyError if cycle detected

        while not scheduler.all_done():
            for task in scheduler.get_ready_tasks():
                pool.submit(task)
    """

    def __init__(self) -> None:
        self._tasks: dict[str, PackageTask] = {}
        self._lock = threading.Lock()

    def add_task(self, task: PackageTask) -> None:
        """Add a task to the scheduler.

        Args:
            task: The package task to schedule.

        Raises:
            ValueError: If a task with the same name already exists.
        """
        with self._lock:
            if task.name in self._tasks:
                raise ValueError(f"Duplicate task name: {task.name}")
            self._tasks[task.name] = task

    def validate(self) -> None:
        """Validate the dependency graph.

        Checks:
        1. All dependency references point to existing tasks
        2. No cyclic dependencies exist

        Raises:
            ValueError: If a dependency references a non-existent task.
            CyclicDependencyError: If the dependency graph contains a cycle.
        """
        with self._lock:
            self._validate_references()
            self._detect_cycles()

    def _validate_references(self) -> None:
        """Check that all dependency names reference existing tasks."""
        for task in self._tasks.values():
            for dep_name in task.dependencies:
                if dep_name not in self._tasks:
                    raise ValueError(f"Task '{task.name}' depends on unknown task '{dep_name}'")

    def _detect_cycles(self) -> None:
        """Detect cycles using DFS with coloring (white/gray/black).

        Raises:
            CyclicDependencyError: If a cycle is detected.
        """
        WHITE, GRAY, BLACK = 0, 1, 2
        color: dict[str, int] = {name: WHITE for name in self._tasks}

        def dfs(name: str, path: list[str]) -> None:
            color[name] = GRAY
            path.append(name)
            task = self._tasks[name]
            for dep_name in task.dependencies:
                if color[dep_name] == GRAY:
                    # Found a back edge - cycle detected
                    cycle_start = path.index(dep_name)
                    cycle = path[cycle_start:] + [dep_name]
                    raise CyclicDependencyError(f"Cyclic dependency detected: {' -> '.join(cycle)}")
                if color[dep_name] == WHITE:
                    dfs(dep_name, path)
            path.pop()
            color[name] = BLACK

        for name in self._tasks:
            if color[name] == WHITE:
                dfs(name, [])

    def get_ready_tasks(self) -> list[PackageTask]:
        """Return tasks whose dependencies are all DONE and that are still WAITING.

        Returns:
            List of tasks ready to begin processing.
        """
        with self._lock:
            ready = []
            for task in self._tasks.values():
                if task.phase != TaskPhase.WAITING:
                    continue
                if self._deps_satisfied(task):
                    ready.append(task)
            return ready

    def _deps_satisfied(self, task: PackageTask) -> bool:
        """Check if all dependencies of a task are DONE."""
        for dep_name in task.dependencies:
            dep_task = self._tasks.get(dep_name)
            if dep_task is None or dep_task.phase != TaskPhase.DONE:
                return False
        return True

    def mark_phase(self, task_name: str, phase: TaskPhase) -> None:
        """Update a task's phase.

        Thread-safe: can be called from pool worker threads.

        Args:
            task_name: Name of the task to update.
            phase: New phase to set.

        Raises:
            KeyError: If the task name doesn't exist.
        """
        with self._lock:
            if task_name not in self._tasks:
                raise KeyError(f"Unknown task: {task_name}")
            self._tasks[task_name].phase = phase

    def get_task(self, task_name: str) -> PackageTask:
        """Get a task by name.

        Args:
            task_name: Name of the task.

        Returns:
            The PackageTask instance.

        Raises:
            KeyError: If the task name doesn't exist.
        """
        with self._lock:
            if task_name not in self._tasks:
                raise KeyError(f"Unknown task: {task_name}")
            return self._tasks[task_name]

    def all_done(self) -> bool:
        """Check if all tasks are in a terminal state (DONE or FAILED).

        Returns:
            True if every task is either DONE or FAILED.
        """
        with self._lock:
            return all(t.phase in (TaskPhase.DONE, TaskPhase.FAILED) for t in self._tasks.values())

    def has_failed(self) -> bool:
        """Check if any task has failed.

        Returns:
            True if at least one task is in FAILED state.
        """
        with self._lock:
            return any(t.phase == TaskPhase.FAILED for t in self._tasks.values())

    def get_blocked_tasks(self) -> list[PackageTask]:
        """Return tasks that are WAITING but blocked by a FAILED dependency.

        These tasks can never complete and should be marked as failed.

        Returns:
            List of tasks that are blocked by failed dependencies.
        """
        with self._lock:
            blocked = []
            for task in self._tasks.values():
                if task.phase != TaskPhase.WAITING:
                    continue
                for dep_name in task.dependencies:
                    dep_task = self._tasks.get(dep_name)
                    if dep_task is not None and dep_task.phase == TaskPhase.FAILED:
                        blocked.append(task)
                        break
            return blocked

    def get_all_tasks(self) -> list[PackageTask]:
        """Return all tasks in the scheduler.

        Returns:
            List of all registered tasks.
        """
        with self._lock:
            return list(self._tasks.values())

    @property
    def task_count(self) -> int:
        """Total number of tasks."""
        with self._lock:
            return len(self._tasks)

    def to_dict(self) -> dict[str, Any]:
        """Serialize scheduler state to dictionary."""
        with self._lock:
            return {
                "tasks": {name: task.to_dict() for name, task in self._tasks.items()},
            }
