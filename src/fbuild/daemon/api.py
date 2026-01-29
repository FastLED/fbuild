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
    - Spawning daemon if needed
    - Waiting for daemon to be ready (HTTP available)
    - Preventing race conditions with atomic locking

    Returns:
        DaemonResponse with status, PID, and launcher info
    """
    with atomic_singleton_lock():
        # Check if daemon already running (HTTP health check)
        if is_daemon_http_available():
            # Daemon is running and HTTP server is responding
            # Read PID file for metadata (may not exist in dev mode)
            pid = read_pid_file() if is_daemon_alive() else None
            launcher = get_launcher_pid() if is_daemon_alive() else None
            return DaemonResponse(status=DaemonStatus.ALREADY_RUNNING, pid=pid, launched_by=launcher, message=f"Daemon already running (HTTP available, PID {pid})")

        # Spawn daemon
        launcher_pid = os.getpid()
        try:
            daemon_pid = spawn_daemon_process(launcher_pid)
        except KeyboardInterrupt:
            raise
        except Exception as e:
            return DaemonResponse(status=DaemonStatus.FAILED, pid=None, launched_by=None, message=f"Failed to spawn daemon: {e}")

        # Wait for daemon to write PID file (legacy compatibility)
        try:
            wait_for_pid_file(expected_pid=daemon_pid, timeout=10)
        except TimeoutError as e:
            return DaemonResponse(status=DaemonStatus.FAILED, pid=daemon_pid, launched_by=launcher_pid, message=f"Daemon started but failed to write PID: {e}")

        # Wait for daemon HTTP server to become available
        if not wait_for_daemon_http(timeout=10.0):
            return DaemonResponse(status=DaemonStatus.FAILED, pid=daemon_pid, launched_by=launcher_pid, message="Daemon started but HTTP server not available after 10s")

        # Lock released ONLY after HTTP server is ready
        return DaemonResponse(status=DaemonStatus.STARTED, pid=daemon_pid, launched_by=launcher_pid, message=f"Daemon started successfully (PID {daemon_pid}, HTTP ready)")


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
