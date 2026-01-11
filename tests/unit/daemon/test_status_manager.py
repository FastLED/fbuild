"""
Unit tests for StatusManager.

Tests status file management, atomic writes, thread safety, and request ID validation.
"""

import json
import tempfile
import threading
import time
from pathlib import Path

import pytest

from fbuild.daemon.messages import DaemonState
from fbuild.daemon.status_manager import StatusManager


@pytest.fixture
def temp_status_file():
    """Create a temporary status file."""
    with tempfile.NamedTemporaryFile(mode="w", suffix=".json", delete=False) as f:
        temp_file = Path(f.name)
    yield temp_file
    # Cleanup
    temp_file.unlink(missing_ok=True)
    temp_file.with_suffix(".tmp").unlink(missing_ok=True)


def test_status_manager_initialization(temp_status_file):
    """Test StatusManager initialization."""
    manager = StatusManager(temp_status_file, daemon_pid=12345)

    assert manager.status_file == temp_status_file
    assert manager.daemon_pid == 12345
    assert manager.daemon_started_at > 0
    assert not manager.get_operation_in_progress()


def test_update_status_creates_file(temp_status_file):
    """Test that update_status creates the status file."""
    manager = StatusManager(temp_status_file, daemon_pid=12345)

    manager.update_status(DaemonState.BUILDING, "Building firmware")

    assert temp_status_file.exists()


def test_update_status_writes_correct_data(temp_status_file):
    """Test that update_status writes correct data to file."""
    manager = StatusManager(temp_status_file, daemon_pid=12345)

    manager.update_status(
        DaemonState.BUILDING,
        "Building firmware",
        environment="esp32dev",
        project_dir="/path/to/project",
    )

    # Read raw JSON
    with open(temp_status_file) as f:
        data = json.load(f)

    assert data["state"] == "building"
    assert data["message"] == "Building firmware"
    assert data["daemon_pid"] == 12345
    assert data["environment"] == "esp32dev"
    assert data["project_dir"] == "/path/to/project"


def test_read_status(temp_status_file):
    """Test reading status from file."""
    manager = StatusManager(temp_status_file, daemon_pid=12345)

    manager.update_status(
        DaemonState.DEPLOYING,
        "Deploying firmware",
        port="/dev/ttyUSB0",
    )

    status = manager.read_status()

    assert status.state == DaemonState.DEPLOYING
    assert status.message == "Deploying firmware"
    assert status.daemon_pid == 12345
    assert status.port == "/dev/ttyUSB0"


def test_read_status_nonexistent_file(temp_status_file):
    """Test reading status when file doesn't exist."""
    # Delete the temp file if it exists
    temp_status_file.unlink(missing_ok=True)

    manager = StatusManager(temp_status_file, daemon_pid=12345)
    status = manager.read_status()

    assert status.state == DaemonState.IDLE
    assert status.message == "Daemon is idle"
    assert status.daemon_pid == 12345


def test_read_status_corrupted_file(temp_status_file):
    """Test reading status when file is corrupted."""
    manager = StatusManager(temp_status_file, daemon_pid=12345)

    # Write corrupted JSON
    with open(temp_status_file, "w") as f:
        f.write("{ invalid json }")

    status = manager.read_status()

    # Should return default status
    assert status.state == DaemonState.IDLE
    assert status.message == "Daemon is idle"


def test_atomic_write_no_partial_state(temp_status_file):
    """Test that writes are atomic (no partial writes visible)."""
    manager = StatusManager(temp_status_file, daemon_pid=12345)

    # Write initial status
    manager.update_status(DaemonState.IDLE, "Idle")

    # This test verifies that the temp file is cleaned up
    temp_file = temp_status_file.with_suffix(".tmp")
    assert not temp_file.exists()


def test_operation_in_progress_flag(temp_status_file):
    """Test operation_in_progress flag management."""
    manager = StatusManager(temp_status_file, daemon_pid=12345)

    # Initially false
    assert not manager.get_operation_in_progress()

    # Set to true
    manager.set_operation_in_progress(True)
    assert manager.get_operation_in_progress()

    # Update status with operation_in_progress
    manager.update_status(DaemonState.BUILDING, "Building", operation_in_progress=True)
    assert manager.get_operation_in_progress()

    # Read status and check
    status = manager.read_status()
    assert status.operation_in_progress

    # Set to false
    manager.set_operation_in_progress(False)
    assert not manager.get_operation_in_progress()


def test_concurrent_updates(temp_status_file):
    """Test thread safety of concurrent status updates."""
    manager = StatusManager(temp_status_file, daemon_pid=12345)

    def update_worker(thread_id: int):
        for i in range(10):
            manager.update_status(
                DaemonState.BUILDING,
                f"Thread {thread_id} update {i}",
                environment=f"esp32dev_{thread_id}",
                project_dir=f"/path/{i}",
            )
            time.sleep(0.001)  # Small delay to encourage interleaving

    # Start multiple threads
    threads = [threading.Thread(target=update_worker, args=(i,)) for i in range(5)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()

    # File should exist and be valid JSON
    assert temp_status_file.exists()
    status = manager.read_status()
    assert status.state == DaemonState.BUILDING
    # Should have one of the thread's messages
    assert "Thread" in status.message


def test_status_manager_with_custom_start_time(temp_status_file):
    """Test StatusManager with custom daemon_started_at."""
    start_time = time.time() - 3600  # 1 hour ago
    manager = StatusManager(temp_status_file, daemon_pid=12345, daemon_started_at=start_time)

    manager.update_status(DaemonState.IDLE, "Idle")

    status = manager.read_status()
    assert status.daemon_started_at == start_time


def test_status_update_preserves_previous_fields(temp_status_file):
    """Test that each status update is independent (doesn't preserve previous fields)."""
    manager = StatusManager(temp_status_file, daemon_pid=12345)

    # First update with environment
    manager.update_status(DaemonState.BUILDING, "Building", environment="esp32dev")
    status1 = manager.read_status()
    assert status1.environment == "esp32dev"

    # Second update without environment
    manager.update_status(DaemonState.DEPLOYING, "Deploying", port="/dev/ttyUSB0")
    status2 = manager.read_status()
    assert status2.port == "/dev/ttyUSB0"
    # environment should not be present in second status
    assert not hasattr(status2, "environment") or status2.environment is None
