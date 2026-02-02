"""
Daemon API: Client interface for daemon lifecycle management.

This module provides the ONLY way for clients to interact with daemon spawning.
All spawn logic is centralized here to prevent race conditions.

REFACTORED: Now uses HTTP-based communication with FastAPI daemon instead of
file-based IPC. The daemon is detected via HTTP health checks rather than PID files.
"""

import os
from dataclasses import dataclass
from enum import Enum

from fbuild.daemon.client.http_utils import (
    is_daemon_http_available,
    wait_for_daemon_http,
)
from fbuild.daemon.singleton_manager import (
    atomic_singleton_lock,
    get_launcher_pid,
    is_daemon_alive,
    read_pid_file,
    spawn_daemon_process,
    wait_for_pid_file,
)


class DaemonStatus(Enum):
    """Daemon request response status."""

    ALREADY_RUNNING = "already_running"
    STARTED = "started"
    FAILED = "failed"


@dataclass
class DaemonResponse:
    """Response from daemon request API."""

    status: DaemonStatus
    pid: int | None
    launched_by: int | None
    message: str = ""


def request_daemon() -> DaemonResponse:
    """
    Request daemon to be running. Idempotent and thread-safe.

    This is the ONLY function clients should call to ensure daemon is running.
    It handles:
    - Checking if daemon is already running (via HTTP health check)
    - Spawning daemon if needed (with retry logic and exponential backoff)
    - Waiting for daemon to be ready (HTTP available)
    - Preventing race conditions with atomic locking
    - Gracefully handling PID mismatches from concurrent spawns

    Retry strategy:
    - Maximum 3 spawn attempts
    - Backoff delays: 0s (first attempt), 500ms (2nd), 2s (3rd)
    - Total max additional latency: 2.5s over 3 attempts
    - Retries on: spawn failures, PID file timeout, HTTP unavailability

    Returns:
        DaemonResponse with status, PID, and launcher info
    """
    import logging
    import time

    # Retry configuration for daemon spawn
    # This handles Windows process creation race conditions where Popen() returns a PID
    # before the process fully initializes. Some processes crash during DLL loading or
    # interpreter startup, so we retry with exponential backoff.
    MAX_SPAWN_ATTEMPTS = 3
    BACKOFF_DELAYS = [0.0, 0.5, 2.0]  # Seconds before each attempt (0s, 500ms, 2s)

    with atomic_singleton_lock():
        # Check if daemon already running (HTTP health check)
        if is_daemon_http_available():
            # Daemon is running and HTTP server is responding
            # Read PID file for metadata (may not exist in dev mode)
            # Note: Don't guard with is_daemon_alive() - we already know daemon is alive via HTTP
            # The is_daemon_alive() check can fail due to PID file timing issues
            pid = read_pid_file()
            launcher = get_launcher_pid()
            return DaemonResponse(status=DaemonStatus.ALREADY_RUNNING, pid=pid, launched_by=launcher, message=f"Daemon already running (HTTP available, PID {pid})")

        launcher_pid = os.getpid()
        last_error = None

        # Retry loop with exponential backoff
        for attempt in range(MAX_SPAWN_ATTEMPTS):
            # Backoff delay (0s for first attempt, then 500ms, 2s)
            if attempt > 0:
                delay = BACKOFF_DELAYS[attempt]
                logging.warning(f"Daemon spawn attempt {attempt + 1}/{MAX_SPAWN_ATTEMPTS} (backing off {delay}s after previous failure)...")
                time.sleep(delay)

            # Spawn daemon
            expected_pid = None
            try:
                expected_pid = spawn_daemon_process(launcher_pid)
            except KeyboardInterrupt:
                raise
            except Exception as e:
                last_error = e
                logging.warning(f"Spawn attempt {attempt + 1}/{MAX_SPAWN_ATTEMPTS} failed to launch process: {e}")
                continue  # Retry

            # Wait for daemon to write PID file - accept any alive daemon (not just expected PID)
            actual_pid = None
            try:
                actual_pid = wait_for_pid_file(expected_pid=expected_pid, timeout=10)
            except TimeoutError as e:
                last_error = e
                # PID file wait failed - check if daemon HTTP is available as fallback
                logging.warning(f"PID file wait failed for expected PID {expected_pid}: {e}")

                if is_daemon_http_available():
                    # Daemon is HTTP-available despite PID file wait failure
                    actual_pid = read_pid_file()
                    if actual_pid:
                        logging.warning(f"PID file wait failed for {expected_pid}, but daemon {actual_pid} is HTTP-available (graceful recovery from race condition)")
                        success_msg = f"Daemon started (HTTP available, expected PID {expected_pid} but got {actual_pid}"
                        if attempt > 0:
                            success_msg += f", succeeded on attempt {attempt + 1})"
                        else:
                            success_msg += ")"
                        return DaemonResponse(
                            status=DaemonStatus.STARTED,
                            pid=actual_pid,
                            launched_by=launcher_pid,
                            message=success_msg,
                        )

                # Daemon not HTTP-available - will retry
                logging.warning(f"Attempt {attempt + 1}/{MAX_SPAWN_ATTEMPTS}: PID file wait failed and HTTP not available")
                continue  # Retry

            # Use actual_pid (not expected_pid) for HTTP wait
            if not wait_for_daemon_http(timeout=10.0):
                last_error = TimeoutError("HTTP server not available after 10s")
                logging.warning(f"Attempt {attempt + 1}/{MAX_SPAWN_ATTEMPTS}: Daemon PID {actual_pid} started but HTTP not available")
                continue  # Retry

            # SUCCESS - Lock released ONLY after HTTP server is ready
            message = f"Daemon started successfully (PID {actual_pid}, HTTP ready"
            if attempt > 0:
                message += f", succeeded on attempt {attempt + 1})"
            else:
                message += ")"
            if actual_pid != expected_pid:
                message += f" [expected {expected_pid}, got {actual_pid} from concurrent spawn]"

            return DaemonResponse(status=DaemonStatus.STARTED, pid=actual_pid, launched_by=launcher_pid, message=message)

        # All attempts failed
        return DaemonResponse(
            status=DaemonStatus.FAILED,
            pid=None,
            launched_by=launcher_pid,
            message=f"Failed to start daemon after {MAX_SPAWN_ATTEMPTS} attempts: {last_error}",
        )


def get_daemon_info() -> DaemonResponse:
    """
    Get current daemon status without spawning.

    Uses HTTP health check to determine if daemon is running.

    Returns:
        DaemonResponse with current status (no spawn attempt)
    """
    if is_daemon_http_available():
        # Daemon is running and HTTP server is responding
        pid = read_pid_file() if is_daemon_alive() else None
        launcher = get_launcher_pid() if is_daemon_alive() else None
        return DaemonResponse(status=DaemonStatus.ALREADY_RUNNING, pid=pid, launched_by=launcher, message=f"Daemon running (HTTP available, PID {pid})")
    else:
        return DaemonResponse(status=DaemonStatus.FAILED, pid=None, launched_by=None, message="No daemon running (HTTP not available)")
