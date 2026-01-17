"""
Lock contention unit tests for ResourceLockManager.

These tests verify the locking mechanism works correctly for both
project locks and port locks under concurrent access.

No hardware required - tests lock manager directly.
"""

import threading
import time
from typing import Any

import pytest

pytestmark = pytest.mark.concurrent


class TestProjectLockContention:
    """Tests for project lock contention scenarios."""

    def test_project_lock_blocks_second_build_non_blocking(self, lock_manager: Any) -> None:
        """Thread 1 holds project lock, Thread 2 tries with blocking=False.

        Thread 2 should get RuntimeError immediately.
        """
        project_dir = "/test/project"
        results: dict[str, Any] = {}
        errors: dict[str, Exception] = {}

        def thread1_work() -> None:
            with lock_manager.acquire_project_lock(project_dir, blocking=True):
                results["thread1_acquired"] = True
                time.sleep(0.5)  # Hold lock for a bit
                results["thread1_done"] = True

        def thread2_work() -> None:
            time.sleep(0.1)  # Let thread1 acquire first
            try:
                with lock_manager.acquire_project_lock(project_dir, blocking=False):
                    results["thread2_acquired"] = True
            except RuntimeError as e:
                errors["thread2"] = e
                results["thread2_failed"] = True

        t1 = threading.Thread(target=thread1_work)
        t2 = threading.Thread(target=thread2_work)

        t1.start()
        t2.start()
        t1.join(timeout=5)
        t2.join(timeout=5)

        # Thread 1 should have succeeded
        assert results.get("thread1_acquired") is True
        assert results.get("thread1_done") is True

        # Thread 2 should have failed with RuntimeError
        assert results.get("thread2_failed") is True
        assert "thread2" in errors
        assert isinstance(errors["thread2"], RuntimeError)
        assert project_dir in str(errors["thread2"])

    def test_project_lock_blocking_mode_waits(self, lock_manager: Any) -> None:
        """Thread 1 holds lock 0.5s, Thread 2 with blocking=True waits.

        Thread 2 should succeed after Thread 1 releases.
        """
        project_dir = "/test/project"
        results: dict[str, Any] = {}
        timings: dict[str, float] = {}

        def thread1_work() -> None:
            timings["t1_start"] = time.time()
            with lock_manager.acquire_project_lock(project_dir, blocking=True):
                results["thread1_acquired"] = True
                time.sleep(0.5)
                timings["t1_release"] = time.time()
                results["thread1_done"] = True

        def thread2_work() -> None:
            time.sleep(0.1)  # Let thread1 acquire first
            timings["t2_start"] = time.time()
            with lock_manager.acquire_project_lock(project_dir, blocking=True):
                timings["t2_acquired"] = time.time()
                results["thread2_acquired"] = True
            results["thread2_done"] = True

        t1 = threading.Thread(target=thread1_work)
        t2 = threading.Thread(target=thread2_work)

        t1.start()
        t2.start()
        t1.join(timeout=5)
        t2.join(timeout=5)

        # Both should succeed
        assert results.get("thread1_done") is True
        assert results.get("thread2_done") is True
        assert results.get("thread2_acquired") is True

        # Thread 2 should have waited for Thread 1
        wait_time = timings["t2_acquired"] - timings["t2_start"]
        assert wait_time >= 0.3  # Should have waited at least some time

    def test_project_lock_released_on_exception(self, lock_manager: Any) -> None:
        """Lock should be released even if an exception occurs inside context."""
        project_dir = "/test/project"

        # Acquire and release with exception
        with pytest.raises(ValueError):
            with lock_manager.acquire_project_lock(project_dir, blocking=True):
                raise ValueError("Test exception")

        # Lock should be released - non-blocking acquire should succeed
        with lock_manager.acquire_project_lock(project_dir, blocking=False):
            pass  # Should not raise

    def test_project_lock_reentrant_same_thread_fails(self, lock_manager: Any) -> None:
        """Attempting to acquire the same lock twice in same thread should deadlock.

        Note: threading.Lock is not reentrant, so blocking=True would deadlock.
        We test with blocking=False to verify the lock is held.
        """
        project_dir = "/test/project"

        with lock_manager.acquire_project_lock(project_dir, blocking=True):
            # Try to acquire again with non-blocking - should fail
            with pytest.raises(RuntimeError):
                with lock_manager.acquire_project_lock(project_dir, blocking=False):
                    pass


class TestPortLockContention:
    """Tests for port lock contention scenarios."""

    def test_port_lock_blocks_second_monitor_non_blocking(self, lock_manager: Any) -> None:
        """Thread 1 holds port lock, Thread 2 tries with blocking=False.

        Thread 2 should get RuntimeError immediately.
        """
        port = "COM3"
        results: dict[str, Any] = {}
        errors: dict[str, Exception] = {}

        def thread1_work() -> None:
            with lock_manager.acquire_port_lock(port, blocking=True):
                results["thread1_acquired"] = True
                time.sleep(0.5)
                results["thread1_done"] = True

        def thread2_work() -> None:
            time.sleep(0.1)  # Let thread1 acquire first
            try:
                with lock_manager.acquire_port_lock(port, blocking=False):
                    results["thread2_acquired"] = True
            except RuntimeError as e:
                errors["thread2"] = e
                results["thread2_failed"] = True

        t1 = threading.Thread(target=thread1_work)
        t2 = threading.Thread(target=thread2_work)

        t1.start()
        t2.start()
        t1.join(timeout=5)
        t2.join(timeout=5)

        # Thread 1 should have succeeded
        assert results.get("thread1_acquired") is True
        assert results.get("thread1_done") is True

        # Thread 2 should have failed
        assert results.get("thread2_failed") is True
        assert "thread2" in errors
        assert isinstance(errors["thread2"], RuntimeError)
        assert port in str(errors["thread2"])

    def test_port_lock_blocking_mode_waits(self, lock_manager: Any) -> None:
        """Thread 1 holds port lock 0.5s, Thread 2 with blocking=True waits."""
        port = "/dev/ttyUSB0"
        results: dict[str, Any] = {}
        timings: dict[str, float] = {}

        def thread1_work() -> None:
            with lock_manager.acquire_port_lock(port, blocking=True):
                results["thread1_acquired"] = True
                time.sleep(0.5)
                timings["t1_release"] = time.time()
                results["thread1_done"] = True

        def thread2_work() -> None:
            time.sleep(0.1)  # Let thread1 acquire first
            timings["t2_start"] = time.time()
            with lock_manager.acquire_port_lock(port, blocking=True):
                timings["t2_acquired"] = time.time()
                results["thread2_acquired"] = True
            results["thread2_done"] = True

        t1 = threading.Thread(target=thread1_work)
        t2 = threading.Thread(target=thread2_work)

        t1.start()
        t2.start()
        t1.join(timeout=5)
        t2.join(timeout=5)

        # Both should succeed
        assert results.get("thread1_done") is True
        assert results.get("thread2_done") is True

        # Thread 2 should have waited
        wait_time = timings["t2_acquired"] - timings["t2_start"]
        assert wait_time >= 0.3

    def test_port_lock_released_on_exception(self, lock_manager: Any) -> None:
        """Port lock should be released even if an exception occurs."""
        port = "COM4"

        with pytest.raises(ValueError):
            with lock_manager.acquire_port_lock(port, blocking=True):
                raise ValueError("Test exception")

        # Lock should be released
        with lock_manager.acquire_port_lock(port, blocking=False):
            pass


class TestMixedLockContention:
    """Tests for scenarios involving both project and port locks."""

    def test_project_and_port_locks_independent(self, lock_manager: Any) -> None:
        """Project lock and port lock should be independent."""
        project_dir = "/test/project"
        port = "COM3"

        # Acquire project lock
        with lock_manager.acquire_project_lock(project_dir, blocking=True):
            # Should be able to acquire port lock independently
            with lock_manager.acquire_port_lock(port, blocking=False):
                pass

    def test_multiple_ports_independent(self, lock_manager: Any) -> None:
        """Different ports should have independent locks."""
        port1 = "COM3"
        port2 = "COM4"

        with lock_manager.acquire_port_lock(port1, blocking=True):
            # Should be able to acquire different port
            with lock_manager.acquire_port_lock(port2, blocking=False):
                pass

    def test_multiple_projects_independent(self, lock_manager: Any) -> None:
        """Different projects should have independent locks."""
        project1 = "/test/project1"
        project2 = "/test/project2"

        with lock_manager.acquire_project_lock(project1, blocking=True):
            # Should be able to acquire different project
            with lock_manager.acquire_project_lock(project2, blocking=False):
                pass

    def test_concurrent_different_resources_all_succeed(self, lock_manager: Any) -> None:
        """Multiple threads acquiring different resources should all succeed."""
        results: dict[str, bool] = {}

        def acquire_project1() -> None:
            with lock_manager.acquire_project_lock("/project1", blocking=True):
                time.sleep(0.2)
                results["project1"] = True

        def acquire_project2() -> None:
            with lock_manager.acquire_project_lock("/project2", blocking=True):
                time.sleep(0.2)
                results["project2"] = True

        def acquire_port1() -> None:
            with lock_manager.acquire_port_lock("COM1", blocking=True):
                time.sleep(0.2)
                results["port1"] = True

        def acquire_port2() -> None:
            with lock_manager.acquire_port_lock("COM2", blocking=True):
                time.sleep(0.2)
                results["port2"] = True

        threads = [
            threading.Thread(target=acquire_project1),
            threading.Thread(target=acquire_project2),
            threading.Thread(target=acquire_port1),
            threading.Thread(target=acquire_port2),
        ]

        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=5)

        # All should succeed
        assert results.get("project1") is True
        assert results.get("project2") is True
        assert results.get("port1") is True
        assert results.get("port2") is True


class TestLockCleanup:
    """Tests for lock cleanup functionality."""

    def test_cleanup_unused_locks_removes_old_locks(self, lock_manager: Any) -> None:
        """cleanup_unused_locks should remove locks not used recently."""
        # Create and use some locks
        with lock_manager.acquire_project_lock("/old/project", blocking=True):
            pass

        with lock_manager.acquire_port_lock("COM99", blocking=True):
            pass

        # Check locks exist
        status = lock_manager.get_lock_status()
        assert "/old/project" in status["project_locks"]
        assert "COM99" in status["port_locks"]

        # Cleanup with very short threshold (locks are now "old")
        removed = lock_manager.cleanup_unused_locks(older_than=0)

        # Locks should be removed
        assert removed >= 2
        status = lock_manager.get_lock_status()
        assert "/old/project" not in status["project_locks"]
        assert "COM99" not in status["port_locks"]

    def test_get_lock_status_returns_acquisition_counts(self, lock_manager: Any) -> None:
        """get_lock_status should return acquisition counts."""
        # Acquire locks multiple times
        for _ in range(3):
            with lock_manager.acquire_project_lock("/test", blocking=True):
                pass

        for _ in range(2):
            with lock_manager.acquire_port_lock("COM1", blocking=True):
                pass

        status = lock_manager.get_lock_status()
        assert status["project_locks"]["/test"] == 3
        assert status["port_locks"]["COM1"] == 2

    def test_get_lock_count_returns_totals(self, lock_manager: Any) -> None:
        """get_lock_count should return total lock counts."""
        # Create some locks
        with lock_manager.acquire_project_lock("/p1", blocking=True):
            pass
        with lock_manager.acquire_project_lock("/p2", blocking=True):
            pass
        with lock_manager.acquire_port_lock("COM1", blocking=True):
            pass

        counts = lock_manager.get_lock_count()
        assert counts["project_locks"] == 2
        assert counts["port_locks"] == 1
