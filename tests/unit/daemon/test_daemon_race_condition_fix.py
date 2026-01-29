"""
Integration test for daemon race condition fix.

Tests that concurrent clients result in exactly ONE daemon process,
validating the singleton manager implementation.
"""

import multiprocessing
import os
import platform
import subprocess
import time

import pytest

from fbuild.daemon.api import DaemonStatus, get_daemon_info, request_daemon
from fbuild.daemon.paths import DAEMON_DIR, LOCK_FILE, PID_FILE
from fbuild.daemon.singleton_manager import is_daemon_alive, read_pid_file


def cleanup_daemon():
    """Stop any existing daemon and clean up PID file.

    This function implements robust cleanup with the following strategy:
    1. Send graceful shutdown signal
    2. Wait for graceful shutdown (up to 5 seconds)
    3. If daemon doesn't respond, forcefully terminate the process
    4. Force remove PID file and lock file
    5. Verify port is released
    """
    shutdown_file = DAEMON_DIR / "shutdown.signal"

    # Step 1: Send shutdown signal
    if shutdown_file.exists():
        shutdown_file.unlink()
    shutdown_file.touch()

    # Step 2: Wait for graceful shutdown (5 seconds)
    for _ in range(10):  # 10 * 0.5s = 5 seconds
        if not is_daemon_alive() and not PID_FILE.exists():
            # Clean shutdown successful
            if shutdown_file.exists():
                shutdown_file.unlink()
            if LOCK_FILE.exists():
                LOCK_FILE.unlink()
            time.sleep(1)  # Wait for port release
            return
        time.sleep(0.5)

    # Step 3: Forceful termination if daemon didn't shut down gracefully
    if PID_FILE.exists():
        try:
            pid_str = PID_FILE.read_text().strip()
            daemon_pid = int(pid_str.split(",")[0])

            print(f"WARNING: Daemon PID {daemon_pid} didn't respond to shutdown signal, forcefully terminating...")

            # Kill daemon process
            if platform.system() == "Windows":
                subprocess.run(["taskkill", "/F", "/PID", str(daemon_pid)], capture_output=True, timeout=5)
            else:
                try:
                    # Use signal 9 (SIGKILL) on Unix
                    os.kill(daemon_pid, 9)
                except ProcessLookupError:
                    pass  # Process already dead

            # Wait for process to die
            time.sleep(1)

        except Exception as e:
            print(f"WARNING: Failed to forcefully terminate daemon: {e}")

    # Step 4: Force remove PID file and lock file
    if PID_FILE.exists():
        print("WARNING: Force-removing stale PID file")
        try:
            PID_FILE.unlink()
        except Exception as e:
            print(f"WARNING: Failed to remove PID file: {e}")

    if LOCK_FILE.exists():
        try:
            LOCK_FILE.unlink()
        except Exception as e:
            print(f"WARNING: Failed to remove lock file: {e}")

    # Step 5: Remove shutdown signal
    if shutdown_file.exists():
        try:
            shutdown_file.unlink()
        except Exception:
            pass

    # Step 6: Wait for port to be released
    time.sleep(1)  # Give OS time to release port 9876


def spawn_client(_worker_id: int) -> int | None:
    """Simulate client requesting daemon. Returns daemon PID."""
    response = request_daemon()
    return response.pid


@pytest.mark.integration
def test_concurrent_spawns_single_daemon():
    """Test that 10 concurrent clients result in exactly 1 daemon."""
    # Clean up any existing daemon
    cleanup_daemon()

    # Spawn 10 clients concurrently
    num_clients = 10
    with multiprocessing.Pool(num_clients) as pool:
        pids = pool.map(spawn_client, range(num_clients))

    # All clients should report the SAME PID
    pids = [p for p in pids if p is not None]
    unique_pids = set(pids)

    try:
        assert len(unique_pids) == 1, f"Expected 1 daemon, got {len(unique_pids)}: {unique_pids}"
        print(f"✅ All {num_clients} clients reported same daemon PID: {unique_pids}")

        # Verify daemon is actually running
        assert is_daemon_alive(), "Daemon PID file exists but process is not alive"

        # Verify PID matches what clients reported
        daemon_pid = read_pid_file()
        assert daemon_pid in unique_pids, f"Daemon PID {daemon_pid} not in client-reported PIDs {unique_pids}"

    finally:
        # Clean up
        cleanup_daemon()


@pytest.mark.integration
def test_launcher_pid_tracking():
    """Test that daemon reports who launched it."""
    cleanup_daemon()

    try:
        launcher_pid = os.getpid()
        response = request_daemon()

        assert response.status == DaemonStatus.STARTED
        assert response.launched_by == launcher_pid

        # Second client sees original launcher
        response2 = request_daemon()
        assert response2.status == DaemonStatus.ALREADY_RUNNING
        assert response2.launched_by == launcher_pid  # Original launcher

    finally:
        cleanup_daemon()


@pytest.mark.integration
def test_sequential_clients_reuse_daemon():
    """Test that sequential clients reuse the same daemon."""
    cleanup_daemon()

    try:
        # First client spawns daemon
        response1 = request_daemon()
        assert response1.status == DaemonStatus.STARTED
        pid1 = response1.pid

        # Wait a bit to ensure daemon is fully started
        time.sleep(1)

        # Second client reuses daemon
        response2 = request_daemon()
        assert response2.status == DaemonStatus.ALREADY_RUNNING
        assert response2.pid == pid1

        # Third client also reuses daemon
        response3 = request_daemon()
        assert response3.status == DaemonStatus.ALREADY_RUNNING
        assert response3.pid == pid1

        print(f"✅ Sequential clients reused same daemon PID: {pid1}")

    finally:
        cleanup_daemon()


@pytest.mark.integration
def test_get_daemon_info_without_spawn():
    """Test that get_daemon_info doesn't spawn daemon."""
    cleanup_daemon()

    try:
        # Query daemon status without spawning
        response = get_daemon_info()
        assert response.status == DaemonStatus.FAILED
        assert response.pid is None

        # Verify no daemon was spawned
        assert not is_daemon_alive()

    finally:
        cleanup_daemon()


if __name__ == "__main__":
    # Allow running directly for debugging
    test_concurrent_spawns_single_daemon()
    test_launcher_pid_tracking()
    test_sequential_clients_reuse_daemon()
    test_get_daemon_info_without_spawn()
    print("✅ All tests passed!")
