"""Centralized subprocess execution manager for daemon operations.

This module provides a unified interface for executing subprocesses with tracking,
logging, and statistics. All subprocess calls should go through this manager for
consistent error handling and monitoring.
"""

from __future__ import annotations

import logging
import subprocess
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Optional

from ..interrupt_utils import handle_keyboard_interrupt_properly

logger = logging.getLogger(__name__)


@dataclass
class SubprocessExecution:
    """Single subprocess execution with full tracking."""

    execution_id: str
    command: list[str]
    cwd: Optional[Path]
    env: Optional[dict[str, str]]
    timeout: Optional[float]
    returncode: Optional[int] = None
    stdout: Optional[str] = None
    stderr: Optional[str] = None
    start_time: Optional[float] = None
    end_time: Optional[float] = None
    error: Optional[str] = None

    def duration(self) -> Optional[float]:
        """Calculate execution duration in seconds."""
        if self.start_time and self.end_time:
            return self.end_time - self.start_time
        return None

    def success(self) -> bool:
        """Check if execution was successful."""
        return self.returncode == 0 and self.error is None


class SubprocessManager:
    """Centralized subprocess execution manager.

    Provides tracking, logging, and statistics for all subprocess executions
    in the daemon. Thread-safe for concurrent use.
    """

    def __init__(self, max_history: int = 1000):
        """Initialize subprocess manager.

        Args:
            max_history: Maximum number of executions to keep in history
        """
        self.executions: dict[str, SubprocessExecution] = {}
        self.lock = threading.Lock()
        self.max_history = max_history
        self._execution_counter = 0

    def execute(
        self,
        command: list[str],
        cwd: Optional[Path] = None,
        env: Optional[dict[str, str]] = None,
        timeout: Optional[float] = 60,
        capture_output: bool = True,
        check: bool = False,
    ) -> SubprocessExecution:
        """Execute subprocess with tracking.

        Args:
            command: Command and arguments to execute
            cwd: Working directory for subprocess
            env: Environment variables
            timeout: Timeout in seconds (None = no timeout)
            capture_output: Whether to capture stdout/stderr
            check: Whether to raise exception on non-zero exit code

        Returns:
            SubprocessExecution with results and timing information
        """
        # Generate unique execution ID
        with self.lock:
            self._execution_counter += 1
            execution_id = f"subprocess_{int(time.time() * 1000000)}_{self._execution_counter}"

        execution = SubprocessExecution(
            execution_id=execution_id,
            command=command,
            cwd=cwd,
            env=env,
            timeout=timeout,
            start_time=time.time(),
        )

        # Store execution
        with self.lock:
            self.executions[execution_id] = execution
            self._cleanup_old_executions()

        # Log execution start
        cmd_str = " ".join(str(c) for c in command[:3])  # First 3 args
        if len(command) > 3:
            cmd_str += "..."
        logger.debug(f"Subprocess {execution_id}: {cmd_str} (timeout={timeout}s)")

        try:
            # Execute subprocess
            result = subprocess.run(
                command,
                cwd=cwd,
                env=env,
                capture_output=capture_output,
                text=True,
                timeout=timeout,
                check=check,
            )

            execution.returncode = result.returncode
            execution.stdout = result.stdout if capture_output else None
            execution.stderr = result.stderr if capture_output else None
            execution.end_time = time.time()

            # Log result
            duration = execution.duration()
            if result.returncode == 0:
                logger.debug(f"Subprocess {execution_id}: SUCCESS in {duration:.2f}s")
            else:
                logger.warning(f"Subprocess {execution_id}: FAILED with code {result.returncode} in {duration:.2f}s")

        except subprocess.TimeoutExpired as e:
            execution.error = f"Timeout after {timeout}s"
            execution.returncode = -1
            execution.stderr = str(e)
            execution.end_time = time.time()

            logger.error(f"Subprocess {execution_id}: TIMEOUT after {timeout}s")

        except subprocess.CalledProcessError as e:
            execution.error = f"Process failed with exit code {e.returncode}"
            execution.returncode = e.returncode
            execution.stdout = e.stdout if capture_output else None
            execution.stderr = e.stderr if capture_output else None
            execution.end_time = time.time()

            logger.error(f"Subprocess {execution_id}: CalledProcessError: {execution.error}")

        except KeyboardInterrupt as ke:
            handle_keyboard_interrupt_properly(ke)

        except Exception as e:
            execution.error = str(e)
            execution.returncode = -1
            execution.end_time = time.time()

            logger.error(f"Subprocess {execution_id}: Exception: {e}", exc_info=True)

        return execution

    def get_execution(self, execution_id: str) -> Optional[SubprocessExecution]:
        """Get execution by ID.

        Args:
            execution_id: Execution ID to retrieve

        Returns:
            SubprocessExecution if found, None otherwise
        """
        with self.lock:
            return self.executions.get(execution_id)

    def get_statistics(self) -> dict[str, Any]:
        """Get subprocess execution statistics.

        Returns:
            Dictionary with execution counts and statistics
        """
        with self.lock:
            total = len(self.executions)
            successful = sum(1 for e in self.executions.values() if e.success())
            failed = sum(1 for e in self.executions.values() if not e.success())

            # Calculate average duration for successful executions
            successful_durations: list[float] = []
            for e in self.executions.values():
                if e.success():
                    duration = e.duration()
                    if duration is not None:
                        successful_durations.append(duration)
            avg_duration = sum(successful_durations) / len(successful_durations) if successful_durations else 0.0

            return {
                "total_executions": total,
                "successful": successful,
                "failed": failed,
                "average_duration_seconds": round(avg_duration, 3),
            }

    def get_recent_failures(self, count: int = 10) -> list[SubprocessExecution]:
        """Get most recent failed executions.

        Args:
            count: Maximum number of failures to return

        Returns:
            List of failed SubprocessExecution objects
        """
        with self.lock:
            failures = [e for e in self.executions.values() if not e.success()]
            # Sort by end_time descending (most recent first)
            failures.sort(key=lambda e: e.end_time or 0, reverse=True)
            return failures[:count]

    def clear_history(self):
        """Clear all execution history."""
        with self.lock:
            self.executions.clear()
            logger.info("Subprocess execution history cleared")

    def _cleanup_old_executions(self):
        """Remove old executions beyond max_history limit.

        Keeps successful executions to max_history, but always keeps all recent failures.
        """
        if len(self.executions) <= self.max_history:
            return

        # Get all executions sorted by end time
        all_executions = sorted(self.executions.values(), key=lambda e: e.end_time or 0)

        # Keep all failures and recent successes
        successes = [e for e in all_executions if e.success()]

        # Remove oldest successes if we're over limit
        to_remove = len(self.executions) - self.max_history
        if to_remove > 0 and len(successes) > to_remove:
            for execution in successes[:to_remove]:
                del self.executions[execution.execution_id]

            logger.debug(f"Cleaned up {to_remove} old successful subprocess executions")


# Global subprocess manager instance (initialized by daemon)
_subprocess_manager: Optional[SubprocessManager] = None


def get_subprocess_manager() -> SubprocessManager:
    """Get global subprocess manager instance.

    Returns:
        Global SubprocessManager instance

    Raises:
        RuntimeError: If subprocess manager not initialized
    """
    global _subprocess_manager
    if _subprocess_manager is None:
        raise RuntimeError("SubprocessManager not initialized. Call init_subprocess_manager() first.")
    return _subprocess_manager


def init_subprocess_manager(max_history: int = 1000) -> SubprocessManager:
    """Initialize global subprocess manager.

    Args:
        max_history: Maximum number of executions to keep in history

    Returns:
        Initialized SubprocessManager instance
    """
    global _subprocess_manager
    _subprocess_manager = SubprocessManager(max_history=max_history)
    logger.info("SubprocessManager initialized")
    return _subprocess_manager
