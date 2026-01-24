"""Unit tests for client cancellation detection."""

import os
import tempfile
import time
from pathlib import Path

import pytest

from fbuild.daemon.cancellation import (
    CancellationReason,
    CancellationRegistry,
    OperationCancelledException,
    check_and_raise_if_cancelled,
)


@pytest.fixture
def temp_daemon_dir():
    """Create a temporary daemon directory for tests."""
    with tempfile.TemporaryDirectory() as tmpdir:
        yield Path(tmpdir)


@pytest.fixture
def registry(temp_daemon_dir):
    """Create a CancellationRegistry for tests."""
    return CancellationRegistry(daemon_dir=temp_daemon_dir, cache_ttl=0.1)


def test_signal_file_detection(registry, temp_daemon_dir):
    """Test detection of cancellation via signal file."""
    request_id = "test_123"
    caller_pid = os.getpid()

    # Initially not cancelled
    reason = registry.check_cancellation(request_id, caller_pid)
    assert reason == CancellationReason.NOT_CANCELLED

    # Create signal file
    signal_file = temp_daemon_dir / f"cancel_{request_id}.signal"
    signal_file.touch()

    # Clear cache to force fresh check
    registry.clear_cache()

    # Should detect cancellation
    reason = registry.check_cancellation(request_id, caller_pid)
    assert reason == CancellationReason.SIGNAL_FILE


def test_process_death_detection(registry):
    """Test detection of cancelled operation when process dies."""
    request_id = "test_456"
    # Use a PID that definitely doesn't exist
    dead_pid = 99999999

    # Should detect process death
    reason = registry.check_cancellation(request_id, dead_pid)
    assert reason == CancellationReason.PROCESS_DEAD


def test_cache_ttl(registry, temp_daemon_dir):
    """Test that cache expires after TTL."""
    request_id = "test_789"
    caller_pid = os.getpid()

    # First check - not cancelled
    reason1 = registry.check_cancellation(request_id, caller_pid)
    assert reason1 == CancellationReason.NOT_CANCELLED

    # Create signal file
    signal_file = temp_daemon_dir / f"cancel_{request_id}.signal"
    signal_file.touch()

    # Immediately check again - cache hit, still shows NOT_CANCELLED
    reason2 = registry.check_cancellation(request_id, caller_pid)
    assert reason2 == CancellationReason.NOT_CANCELLED

    # Wait for cache to expire (cache_ttl = 0.1s)
    time.sleep(0.15)

    # Now should detect signal file
    reason3 = registry.check_cancellation(request_id, caller_pid)
    assert reason3 == CancellationReason.SIGNAL_FILE


def test_cleanup_signal_file(registry, temp_daemon_dir):
    """Test cleanup of signal files."""
    request_id = "test_cleanup"
    signal_file = temp_daemon_dir / f"cancel_{request_id}.signal"

    # Create signal file
    signal_file.touch()
    assert signal_file.exists()

    # Cleanup
    registry.cleanup_signal_file(request_id)
    assert not signal_file.exists()

    # Cleanup non-existent file should not error
    registry.cleanup_signal_file(request_id)


def test_clear_cache(registry):
    """Test clearing the cache."""
    request_id = "test_cache"
    caller_pid = os.getpid()

    # Populate cache
    registry.check_cancellation(request_id, caller_pid)

    # Clear cache
    registry.clear_cache()

    # Cache should be empty (test by checking internal state)
    with registry._lock:
        assert len(registry._check_cache) == 0


def test_check_and_raise_for_cancellable_operation(registry, temp_daemon_dir):
    """Test check_and_raise_if_cancelled for CANCELLABLE operation."""
    request_id = "test_raise"
    caller_pid = os.getpid()

    # Create signal file
    signal_file = temp_daemon_dir / f"cancel_{request_id}.signal"
    signal_file.touch()

    # Clear cache to force fresh check
    registry.clear_cache()

    # Should raise OperationCancelledException for build operation
    with pytest.raises(OperationCancelledException) as exc_info:
        check_and_raise_if_cancelled(registry, request_id, caller_pid, "build")

    assert exc_info.value.reason == CancellationReason.SIGNAL_FILE


def test_check_and_raise_for_continue_operation(registry, temp_daemon_dir):
    """Test check_and_raise_if_cancelled for CONTINUE operation."""
    request_id = "test_continue"
    caller_pid = os.getpid()

    # Create signal file
    signal_file = temp_daemon_dir / f"cancel_{request_id}.signal"
    signal_file.touch()

    # Clear cache to force fresh check
    registry.clear_cache()

    # Should NOT raise for install_dependencies (CONTINUE policy)
    # This should complete without raising
    check_and_raise_if_cancelled(registry, request_id, caller_pid, "install_dependencies")


def test_check_and_raise_when_not_cancelled(registry):
    """Test check_and_raise_if_cancelled when operation not cancelled."""
    request_id = "test_not_cancelled"
    caller_pid = os.getpid()

    # Should not raise
    check_and_raise_if_cancelled(registry, request_id, caller_pid, "build")


def test_alive_process_not_cancelled(registry):
    """Test that alive process is not flagged as cancelled."""
    request_id = "test_alive"
    caller_pid = os.getpid()  # Current process is alive

    reason = registry.check_cancellation(request_id, caller_pid)
    assert reason == CancellationReason.NOT_CANCELLED
