"""Client cancellation detection for daemon operations.

This module provides dual-channel cancellation detection:
1. Signal files - Explicit user cancellation (Ctrl+C)
2. Process checks - Detects client crashes/kills

Features:
- Caching with 100ms TTL to minimize psutil overhead
- Thread-safe (uses lock for cache access)
- Operation-specific policies (CANCELLABLE vs CONTINUE)
"""

import logging
import time
from dataclasses import dataclass
from enum import Enum
from pathlib import Path

import psutil

logger = logging.getLogger(__name__)


class CancellationPolicy(Enum):
    """Policy for handling client disconnection."""

    CANCELLABLE = "cancellable"  # Cancel operation when client disconnects
    CONTINUE = "continue"  # Continue despite client disconnect


class CancellationReason(Enum):
    """Reason for cancellation."""

    SIGNAL_FILE = "signal_file"  # Client created cancel signal
    PROCESS_DEAD = "process_dead"  # Client process no longer exists
    NOT_CANCELLED = "not_cancelled"  # Operation should continue


@dataclass
class CancellationCheck:
    """Result of cancellation check with caching."""

    reason: CancellationReason
    timestamp: float
    message: str


class OperationCancelledException(Exception):
    """Raised when operation cancelled due to client disconnect."""

    def __init__(self, reason: CancellationReason, message: str):
        self.reason = reason
        super().__init__(message)


class CancellationRegistry:
    """Registry tracking cancellable operations with dual-channel detection.

    Detection channels:
    1. Signal files - Explicit user cancellation (Ctrl+C)
    2. Process checks - Detects client crashes/kills

    Features:
    - Caching with 100ms TTL to minimize psutil overhead
    - Thread-safe (uses lock for cache access)
    - Configurable check intervals
    """

    def __init__(self, daemon_dir: Path, cache_ttl: float = 0.1):
        """Initialize registry.

        Args:
            daemon_dir: Directory containing cancel signal files
            cache_ttl: Cache time-to-live in seconds
        """
        self._daemon_dir = daemon_dir
        self._cache_ttl = cache_ttl
        self._check_cache: dict[str, CancellationCheck] = {}
        import threading

        self._lock = threading.Lock()

    def check_cancellation(self, request_id: str, caller_pid: int) -> CancellationReason:
        """Check if operation should be cancelled (with caching).

        Args:
            request_id: Unique request identifier
            caller_pid: Client process ID

        Returns:
            CancellationReason indicating if/why to cancel
        """
        cache_key = f"{request_id}_{caller_pid}"

        # Check cache first
        with self._lock:
            if cache_key in self._check_cache:
                check = self._check_cache[cache_key]
                if (time.time() - check.timestamp) < self._cache_ttl:
                    return check.reason

        # Channel 1: Check signal file (explicit user cancellation)
        signal_file = self._daemon_dir / f"cancel_{request_id}.signal"
        if signal_file.exists():
            reason = CancellationReason.SIGNAL_FILE
            message = "Client requested cancellation (signal file)"
            with self._lock:
                self._check_cache[cache_key] = CancellationCheck(reason, time.time(), message)
            logger.info(f"Cancellation detected for {request_id}: {message}")
            return reason

        # Channel 2: Check process existence (crash/kill detection)
        try:
            if not psutil.pid_exists(caller_pid):
                reason = CancellationReason.PROCESS_DEAD
                message = f"Client process {caller_pid} no longer exists"
                with self._lock:
                    self._check_cache[cache_key] = CancellationCheck(reason, time.time(), message)
                logger.info(f"Cancellation detected for {request_id}: {message}")
                return reason
        except KeyboardInterrupt:
            raise
        except Exception as e:
            logger.warning(f"Failed to check process {caller_pid}: {e}")

        # No cancellation detected
        reason = CancellationReason.NOT_CANCELLED
        with self._lock:
            self._check_cache[cache_key] = CancellationCheck(reason, time.time(), "")
        return reason

    def cleanup_signal_file(self, request_id: str) -> None:
        """Remove cancel signal file after handling."""
        signal_file = self._daemon_dir / f"cancel_{request_id}.signal"
        try:
            signal_file.unlink(missing_ok=True)
            logger.debug(f"Cleaned up cancel signal for {request_id}")
        except KeyboardInterrupt:
            raise
        except Exception as e:
            logger.warning(f"Failed to cleanup cancel signal: {e}")

    def clear_cache(self) -> None:
        """Clear the check cache."""
        with self._lock:
            self._check_cache.clear()


# Operation-specific cancellation policies
OPERATION_POLICIES = {
    "build": CancellationPolicy.CANCELLABLE,
    "deploy": CancellationPolicy.CANCELLABLE,
    "monitor": CancellationPolicy.CANCELLABLE,
    "build_and_deploy": CancellationPolicy.CANCELLABLE,
    "install_dependencies": CancellationPolicy.CONTINUE,  # Continue downloads
}


def check_and_raise_if_cancelled(
    registry: CancellationRegistry,
    request_id: str,
    caller_pid: int,
    operation_type: str,
) -> None:
    """Check for cancellation and raise exception if policy allows.

    Args:
        registry: CancellationRegistry instance
        request_id: Request ID to check
        caller_pid: Client process ID
        operation_type: Operation type string

    Raises:
        OperationCancelledException: If should cancel per policy
    """
    reason = registry.check_cancellation(request_id, caller_pid)

    if reason == CancellationReason.NOT_CANCELLED:
        return

    # Get cancellation policy for this operation
    policy = OPERATION_POLICIES.get(operation_type, CancellationPolicy.CANCELLABLE)

    if policy == CancellationPolicy.CONTINUE:
        # Log but don't cancel
        logger.info(f"Client disconnected ({reason.value}) but continuing {operation_type} per policy")
        return

    # Cancel the operation
    message = f"Operation cancelled: {reason.value}"
    logger.info(message)
    raise OperationCancelledException(reason, message)
