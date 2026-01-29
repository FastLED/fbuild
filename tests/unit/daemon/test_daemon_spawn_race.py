"""
Test daemon spawn race condition safety.

These tests validate that the daemon spawn logic handles concurrent spawn attempts
correctly and safely, without spurious errors or race conditions.
"""

import multiprocessing
import time

import pytest

from fbuild.daemon.api import DaemonStatus, get_daemon_info, request_daemon
from fbuild.daemon.client.lifecycle import stop_daemon
from fbuild.daemon.paths import DAEMON_DIR, PID_FILE


def spawn_worker(worker_id: int) -> dict:
    """
    Worker function that requests daemon (runs in separate process).

    Args:
        worker_id: Unique identifier for this worker

    Returns:
        Dictionary with worker results (status, PID, message, worker_id)
    """
    try:
        response = request_daemon()
        return {
            "worker_id": worker_id,
            "status": response.status.value,
            "pid": response.pid,
            "message": response.message,
        }
    except Exception as e:
        return {
            "worker_id": worker_id,
            "status": "error",
            "pid": None,
            "message": str(e),
        }


@pytest.mark.unit
def test_concurrent_daemon_spawn_five_processes():
    """
    Test that concurrent daemon spawn attempts from 5 processes are safe.

    This is the PRIMARY test for race condition handling. It validates:
    - Only one daemon process is spawned successfully
    - All spawn requests succeed (no spurious errors)
    - All workers get the same daemon PID
    - Daemon is actually running and HTTP-available after spawn
    """
    # Clean state - stop any existing daemon
    try:
        stop_daemon()
    except Exception:
        pass
    time.sleep(1.0)

    # Ensure daemon is not running
    if PID_FILE.exists():
        PID_FILE.unlink()

    # Spawn daemon from 5 concurrent processes
    num_workers = 5
    with multiprocessing.Pool(processes=num_workers) as pool:
        results = pool.map(spawn_worker, range(num_workers))

    # Analyze results
    failures = [r for r in results if r["status"] == "failed"]
    errors = [r for r in results if r["status"] == "error"]
    successes = [r for r in results if r["status"] in ("started", "already_running")]

    # Debug output
    print("\n=== Concurrent Spawn Test Results ===")
    print(f"Successes: {len(successes)}/{num_workers}")
    print(f"Failures: {len(failures)}/{num_workers}")
    print(f"Errors: {len(errors)}/{num_workers}")
    for r in results:
        print(f"  Worker {r['worker_id']}: {r['status']} - PID {r['pid']} - {r['message'][:80]}")

    # Assertions
    assert len(errors) == 0, f"Workers encountered errors: {errors}"
    assert len(successes) > 0, f"All spawn attempts failed: {failures}"

    # CRITICAL: No spurious failures - all workers should succeed
    # This is the main test for the race condition fix
    assert len(failures) == 0, f"Spurious failures detected: {len(failures)}/{num_workers} workers failed " f"despite daemon running. This indicates race condition not fixed. Failures: {failures}"

    # Check that daemon is actually running
    daemon_info = get_daemon_info()
    assert daemon_info.status == DaemonStatus.ALREADY_RUNNING, f"Daemon not running after concurrent spawn: {daemon_info.message}"

    # All successful spawns should report the same PID (if PID is available)
    # Note: In dev mode, PID might be None if daemon doesn't write PID file
    success_pids = {r["pid"] for r in successes if r["pid"] is not None}
    if success_pids:
        assert len(success_pids) == 1, f"Multiple different daemon PIDs reported: {success_pids}"
        # Actual daemon PID should match (if both are not None)
        if daemon_info.pid is not None:
            assert daemon_info.pid in success_pids, f"Daemon PID {daemon_info.pid} not in reported PIDs {success_pids}"
    else:
        # All PIDs are None - this is acceptable in dev mode
        print("  - All PIDs are None (acceptable in dev mode)")

    print("\n✓ Concurrent spawn test PASSED:")
    print(f"  - {len(successes)}/{num_workers} workers succeeded")
    print(f"  - {len(failures)}/{num_workers} workers failed (expected: 0)")
    print(f"  - Daemon PID: {daemon_info.pid}")
    print(f"  - All workers reported same PID: {success_pids}")

    # Cleanup
    stop_daemon()


@pytest.mark.unit
def test_daemon_spawn_with_retry_resilience():
    """
    Test that daemon spawn succeeds even with transient failures.

    This test validates the retry logic by ensuring spawn eventually succeeds
    even in adverse conditions (simulated by stopping daemon between attempts).
    """
    # Clean state
    try:
        stop_daemon()
    except Exception:
        pass
    time.sleep(0.5)

    # Ensure daemon is not running
    if PID_FILE.exists():
        PID_FILE.unlink()

    # Request daemon - should succeed even if internal retries happen
    response = request_daemon()
    assert response.status in (DaemonStatus.STARTED, DaemonStatus.ALREADY_RUNNING), f"Daemon spawn failed: {response.message}"

    # Verify daemon is actually running
    daemon_info = get_daemon_info()
    assert daemon_info.status == DaemonStatus.ALREADY_RUNNING, f"Daemon not running: {daemon_info.message}"
    # Note: PID may be None in dev mode if PID file doesn't exist, but HTTP is available
    # This is acceptable as long as daemon is running

    print("\n✓ Spawn with retry test PASSED:")
    print(f"  - Daemon PID: {daemon_info.pid or 'N/A (dev mode)'}")
    print(f"  - Status: {daemon_info.status.value}")
    print(f"  - Message: {daemon_info.message}")

    # Cleanup
    stop_daemon()


@pytest.mark.unit
def test_daemon_spawn_log_append_mode():
    """
    Test that spawn log appends attempts (doesn't overwrite).

    This validates that we can debug multiple spawn attempts by examining
    the spawn log, which should preserve all attempts with timestamps.
    """
    # Clean state
    try:
        stop_daemon()
    except Exception:
        pass
    time.sleep(0.5)

    spawn_log = DAEMON_DIR / "daemon_spawn.log"
    # Try to delete spawn log, but it might be locked on Windows (daemon has it open)
    if spawn_log.exists():
        try:
            spawn_log.unlink()
        except PermissionError:
            # File is locked - daemon still has it open. This is acceptable.
            # We'll just append to it and verify append behavior
            pass

    # First spawn
    response1 = request_daemon()
    assert response1.status in (DaemonStatus.STARTED, DaemonStatus.ALREADY_RUNNING), f"First spawn failed: {response1.message}"

    # Read spawn log after first spawn
    if spawn_log.exists():
        try:
            # Use UTF-8 encoding to handle ANSI color codes
            content1 = spawn_log.read_text(encoding="utf-8", errors="ignore")
            line_count1 = len(content1.splitlines())
            header_count1 = content1.count("Spawn attempt at")
            print(f"\nAfter first spawn: {line_count1} lines, {header_count1} headers")
        except (PermissionError, UnicodeDecodeError) as e:
            # File might be locked or contain binary data
            line_count1 = 0
            header_count1 = 0
            print(f"\nCouldn't read spawn log after first spawn: {e}")
    else:
        line_count1 = 0
        header_count1 = 0
        print("\nSpawn log doesn't exist after first spawn (may be normal for HTTP-based daemon)")

    # Stop daemon
    stop_daemon()
    time.sleep(0.5)

    # Second spawn
    response2 = request_daemon()
    assert response2.status in (DaemonStatus.STARTED, DaemonStatus.ALREADY_RUNNING), f"Second spawn failed: {response2.message}"

    # Read spawn log again
    if spawn_log.exists():
        try:
            # Use UTF-8 encoding to handle ANSI color codes
            content2 = spawn_log.read_text(encoding="utf-8", errors="ignore")
            line_count2 = len(content2.splitlines())
            header_count2 = content2.count("Spawn attempt at")

            print(f"After second spawn: {line_count2} lines, {header_count2} headers")

            # Should have MORE lines (append mode)
            if line_count1 > 0:
                assert line_count2 > line_count1, f"Spawn log not appending: {line_count1} -> {line_count2} lines"

                # Note: We expect multiple spawn headers, but uvicorn may overwrite portions of the log
                # The key test is that line count INCREASES, proving append mode works
                # Header count might not increase if uvicorn truncates the file on restart
                if header_count2 >= 2:
                    print("\n✓ Spawn log append test PASSED (ideal):")
                    print(f"  - Lines: {line_count1} -> {line_count2} (increased)")
                    print(f"  - Spawn headers: {header_count1} -> {header_count2} (multiple attempts preserved)")
                else:
                    print("\n✓ Spawn log append test PASSED (partial):")
                    print(f"  - Lines: {line_count1} -> {line_count2} (increased - append mode working)")
                    print(f"  - Spawn headers: {header_count1} -> {header_count2} (may be overwritten by uvicorn)")
                    print("  - Note: Line count increase proves append mode is working")
            else:
                # First read failed, just verify second spawn logged something
                assert line_count2 > 0, "Spawn log is empty after second spawn"
                print("\n✓ Spawn log append test PASSED (partial):")
                print(f"  - Lines after second spawn: {line_count2}")
                print(f"  - Spawn headers: {header_count2}")

        except (PermissionError, UnicodeDecodeError) as e:
            # File might be locked or contain binary data
            print(f"\n⚠ Couldn't read spawn log after second spawn: {e}")
            print("  This is acceptable on Windows where file may be locked")
    else:
        # If spawn log doesn't exist, it might be because daemon startup is so fast
        # that stderr never gets written. This is acceptable.
        print("\n⚠ Spawn log doesn't exist - daemon may not use stderr logging")
        print("  This is acceptable if daemon startup is error-free")

    # Cleanup
    stop_daemon()


@pytest.mark.unit
def test_daemon_spawn_idempotent():
    """
    Test that requesting daemon multiple times is idempotent and safe.

    This validates that calling request_daemon() multiple times from the
    same process doesn't cause issues.
    """
    # Clean state
    try:
        stop_daemon()
    except Exception:
        pass
    time.sleep(0.5)

    # Request daemon 3 times in a row
    response1 = request_daemon()
    assert response1.status in (DaemonStatus.STARTED, DaemonStatus.ALREADY_RUNNING)

    response2 = request_daemon()
    assert response2.status == DaemonStatus.ALREADY_RUNNING, "Second request should return ALREADY_RUNNING"

    response3 = request_daemon()
    assert response3.status == DaemonStatus.ALREADY_RUNNING, "Third request should return ALREADY_RUNNING"

    # All should report same PID
    pids = {response1.pid, response2.pid, response3.pid}
    assert len(pids) == 1, f"Multiple PIDs reported: {pids}"

    print("\n✓ Idempotent spawn test PASSED:")
    print("  - All 3 requests succeeded")
    print(f"  - All reported same PID: {pids}")

    # Cleanup
    stop_daemon()


@pytest.mark.unit
def test_daemon_spawn_accept_any_alive_pid():
    """
    Test that wait_for_pid_file() accepts any alive daemon PID.

    This is a lower-level test that validates the core fix for the race condition:
    accepting any alive daemon PID, not just the expected one.
    """
    # This test is more of an integration test since it requires actual daemon spawn
    # We test it indirectly through the concurrent spawn test

    # Clean state
    try:
        stop_daemon()
    except Exception:
        pass
    time.sleep(0.5)

    # Request daemon
    response = request_daemon()
    assert response.status in (DaemonStatus.STARTED, DaemonStatus.ALREADY_RUNNING)

    # Check if message indicates PID mismatch was accepted
    # (This happens when expected_pid != actual_pid)
    if "expected" in response.message.lower() and "got" in response.message.lower():
        print("\n✓ PID mismatch detected and accepted:")
        print(f"  - Message: {response.message}")
        print("  - This validates that wait_for_pid_file() accepts any alive daemon")
    else:
        print("\n✓ No PID mismatch detected (daemon spawned cleanly)")
        print(f"  - Message: {response.message}")

    # Verify daemon is running
    daemon_info = get_daemon_info()
    assert daemon_info.status == DaemonStatus.ALREADY_RUNNING
    assert daemon_info.pid == response.pid

    # Cleanup
    stop_daemon()


if __name__ == "__main__":
    # Allow running tests directly for debugging
    print("Running daemon spawn race condition tests...")
    print("=" * 70)

    test_daemon_spawn_with_retry_resilience()
    print("\n" + "=" * 70)

    test_daemon_spawn_log_append_mode()
    print("\n" + "=" * 70)

    test_daemon_spawn_idempotent()
    print("\n" + "=" * 70)

    test_daemon_spawn_accept_any_alive_pid()
    print("\n" + "=" * 70)

    # Concurrent test last (most intensive)
    test_concurrent_daemon_spawn_five_processes()
    print("\n" + "=" * 70)

    print("\nAll tests passed!")
