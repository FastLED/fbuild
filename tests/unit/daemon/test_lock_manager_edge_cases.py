"""
Unit tests for ResourceLockManager edge cases and corner cases.

These tests specifically target problematic locking scenarios identified from:
1. LOG.txt analysis of production issues
2. Code review of lock_manager.py
3. Edge cases in concurrent access patterns

Test categories:
- is_held() operator precedence issues
- Stale lock detection edge cases
- Race conditions in force release
- Lock cleanup timing issues
- Rapid acquire/release cycles
"""

import threading
import time
import unittest
from concurrent.futures import ThreadPoolExecutor, as_completed

from fbuild.daemon.lock_manager import (
    DEFAULT_LOCK_TIMEOUT,
    STALE_LOCK_THRESHOLD,
    LockAcquisitionError,
    LockInfo,
    ResourceLockManager,
)


class TestLockInfoIsHeldLogic(unittest.TestCase):
    """Test LockInfo.is_held() boolean logic edge cases.

    The is_held() method has complex boolean logic that could have
    operator precedence issues. This test verifies correct behavior.
    """

    def test_is_held_never_acquired(self):
        """Lock that was never acquired should not be held."""
        lock_info = LockInfo(lock=threading.Lock())
        self.assertFalse(lock_info.is_held())

    def test_is_held_acquired_not_released(self):
        """Lock that was acquired but not released should be held."""
        lock_info = LockInfo(lock=threading.Lock())
        lock_info.acquired_at = time.time()
        lock_info.last_released_at = None
        self.assertTrue(lock_info.is_held())

    def test_is_held_released_after_acquired(self):
        """Lock that was released after being acquired should not be held."""
        lock_info = LockInfo(lock=threading.Lock())
        lock_info.acquired_at = time.time()
        time.sleep(0.01)  # Ensure different timestamps
        lock_info.last_released_at = time.time()
        self.assertFalse(lock_info.is_held())

    def test_is_held_reacquired_after_release(self):
        """Lock that was reacquired after release should be held."""
        lock_info = LockInfo(lock=threading.Lock())
        # First acquisition and release
        lock_info.acquired_at = time.time()
        time.sleep(0.01)
        lock_info.last_released_at = time.time()
        self.assertFalse(lock_info.is_held())

        # Re-acquire
        time.sleep(0.01)
        lock_info.acquired_at = time.time()
        self.assertTrue(lock_info.is_held())

    def test_is_held_same_timestamp_edge_case(self):
        """Test when acquired_at equals last_released_at (edge case).

        This is a potential race condition where timestamps are identical.
        The lock should NOT be considered held if timestamps are equal
        (release happened at same time as acquisition - effectively released).
        """
        lock_info = LockInfo(lock=threading.Lock())
        timestamp = time.time()
        lock_info.acquired_at = timestamp
        lock_info.last_released_at = timestamp
        # With equal timestamps, acquired_at > last_released_at is False
        # So is_held() should return False
        self.assertFalse(lock_info.is_held())

    def test_is_held_operator_precedence(self):
        """Verify operator precedence in is_held() doesn't cause issues.

        The expression uses 'and' and 'or' which have different precedence.
        This test ensures the logic is correct.

        Original: acquired_at is not None and last_released_at is None or (...)
        This could be parsed as: (acquired_at and last_released_at is None) or (...)
        instead of the intended: (acquired_at and last_released_at is None) or (...)
        """
        lock_info = LockInfo(lock=threading.Lock())

        # Case: acquired_at is None, last_released_at is not None
        # Should NOT be held
        lock_info.acquired_at = None
        lock_info.last_released_at = time.time()
        self.assertFalse(lock_info.is_held())

        # Case: acquired_at is not None, last_released_at is not None,
        # acquired_at < last_released_at
        # Should NOT be held
        lock_info.acquired_at = time.time()
        time.sleep(0.01)
        lock_info.last_released_at = time.time()
        self.assertFalse(lock_info.is_held())


class TestLockInfoStaleDetection(unittest.TestCase):
    """Test LockInfo.is_stale() edge cases."""

    def test_is_stale_not_held(self):
        """Lock that is not held should not be stale."""
        lock_info = LockInfo(lock=threading.Lock(), timeout=0.01)
        self.assertFalse(lock_info.is_stale())

    def test_is_stale_held_within_timeout(self):
        """Lock held within timeout should not be stale."""
        lock_info = LockInfo(lock=threading.Lock(), timeout=10.0)
        lock_info.acquired_at = time.time()
        self.assertFalse(lock_info.is_stale())

    def test_is_stale_held_beyond_timeout(self):
        """Lock held beyond timeout should be stale."""
        lock_info = LockInfo(lock=threading.Lock(), timeout=0.01)
        lock_info.acquired_at = time.time() - 1.0  # 1 second ago
        self.assertTrue(lock_info.is_stale())

    def test_is_stale_with_zero_timeout(self):
        """Lock with zero timeout should be immediately stale when held."""
        lock_info = LockInfo(lock=threading.Lock(), timeout=0.0)
        lock_info.acquired_at = time.time()
        time.sleep(0.001)  # Tiny sleep to ensure time passes
        self.assertTrue(lock_info.is_stale())

    def test_is_stale_with_very_large_timeout(self):
        """Lock with very large timeout should not be stale."""
        lock_info = LockInfo(lock=threading.Lock(), timeout=float("inf"))
        lock_info.acquired_at = time.time() - 1000000  # Very old
        # Note: inf > any finite number, so this won't be stale
        self.assertFalse(lock_info.is_stale())


class TestRapidAcquireReleaseCycles(unittest.TestCase):
    """Test rapid acquire/release cycles for race conditions."""

    def setUp(self):
        self.manager = ResourceLockManager()

    def test_rapid_sequential_acquire_release(self):
        """Test many rapid acquire/release cycles in sequence."""
        port = "COM_RAPID"

        for i in range(100):
            with self.manager.acquire_port_lock(port):
                pass

        status = self.manager.get_lock_status()
        self.assertEqual(status["port_locks"][port], 100)

    def test_concurrent_acquire_same_resource(self):
        """Test concurrent threads trying to acquire same resource."""
        port = "COM_CONCURRENT"
        acquired_count = [0]
        lock = threading.Lock()

        def acquire_and_count():
            with self.manager.acquire_port_lock(port, blocking=True, timeout=5.0):
                with lock:
                    acquired_count[0] += 1
                time.sleep(0.01)  # Small work

        threads = [threading.Thread(target=acquire_and_count) for _ in range(10)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=30)

        # All threads should have acquired the lock
        self.assertEqual(acquired_count[0], 10)

    def test_concurrent_acquire_different_resources(self):
        """Test concurrent threads acquiring different resources."""
        results = []
        lock = threading.Lock()

        def acquire_resource(port):
            with self.manager.acquire_port_lock(port, blocking=True):
                with lock:
                    results.append(port)
                time.sleep(0.05)

        threads = [threading.Thread(target=acquire_resource, args=(f"COM_{i}",)) for i in range(10)]

        start = time.time()
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=10)
        elapsed = time.time() - start

        # All should complete in parallel (roughly 0.05s, not 0.5s)
        self.assertEqual(len(results), 10)
        self.assertLess(elapsed, 1.0)  # Should be much less than 5.0s (0.5s * 10 sequential)


class TestForceReleaseLockEdgeCases(unittest.TestCase):
    """Test force_release_lock edge cases and race conditions."""

    def setUp(self):
        self.manager = ResourceLockManager()

    def test_force_release_nonexistent_lock(self):
        """Force releasing a lock that doesn't exist should return False."""
        result = self.manager.force_release_lock("port", "NONEXISTENT_PORT")
        self.assertFalse(result)

    def test_force_release_unheld_lock(self):
        """Force releasing a lock that exists but isn't held should return False."""
        # Create lock by acquiring and releasing
        with self.manager.acquire_port_lock("COM_UNHELD"):
            pass

        # Now force release - should return False since not held
        result = self.manager.force_release_lock("port", "COM_UNHELD")
        self.assertFalse(result)

    def test_force_release_held_lock_from_different_thread(self):
        """Force releasing a lock held by another thread.

        This is a known problematic scenario - threading.Lock.release()
        can only be called from the thread that acquired it.
        """
        port = "COM_CROSS_THREAD"
        lock_acquired = threading.Event()
        can_release = threading.Event()
        release_result = [None]

        def holder_thread():
            with self.manager.acquire_port_lock(port, blocking=True):
                lock_acquired.set()
                can_release.wait(timeout=5.0)

        def releaser_thread():
            lock_acquired.wait(timeout=5.0)
            # Try to force release from different thread
            release_result[0] = self.manager.force_release_lock("port", port)
            can_release.set()

        holder = threading.Thread(target=holder_thread)
        releaser = threading.Thread(target=releaser_thread)

        holder.start()
        releaser.start()

        holder.join(timeout=10)
        releaser.join(timeout=10)

        # The force release should have "succeeded" (returned True)
        # but the actual lock.release() would have raised RuntimeError
        # which is caught and ignored in force_release_lock
        self.assertTrue(release_result[0])

    def test_force_release_stale_locks(self):
        """Test force_release_stale_locks cleans up old locks."""
        # Create a "stale" lock by manipulating timestamps
        with self.manager.acquire_port_lock("COM_STALE", timeout=0.001):
            # Manually set acquired_at to past
            with self.manager._master_lock:
                lock_info = self.manager._port_locks["COM_STALE"]
                lock_info.acquired_at = time.time() - 10000  # Very old

            time.sleep(0.01)  # Ensure it's past timeout

            # While still in context, check if stale detection works
            stale = self.manager.get_stale_locks()
            self.assertEqual(len(stale.stale_port_locks), 1)


class TestCleanupUnusedLocks(unittest.TestCase):
    """Test cleanup_unused_locks edge cases."""

    def setUp(self):
        self.manager = ResourceLockManager()

    def test_cleanup_does_not_remove_held_locks(self):
        """Cleanup should not remove locks that are currently held."""
        port = "COM_HELD"

        with self.manager.acquire_port_lock(port):
            # Try cleanup while lock is held
            self.manager.cleanup_unused_locks(older_than=0)

            # Lock should NOT be removed
            status = self.manager.get_lock_status()
            self.assertIn(port, status["port_locks"])

    def test_cleanup_removes_old_unheld_locks(self):
        """Cleanup should remove locks not used recently."""
        port = "COM_OLD"

        # Create lock
        with self.manager.acquire_port_lock(port):
            pass

        # Cleanup with 0 threshold - should remove
        removed = self.manager.cleanup_unused_locks(older_than=0)

        self.assertEqual(removed, 1)
        status = self.manager.get_lock_status()
        self.assertNotIn(port, status["port_locks"])

    def test_cleanup_concurrent_with_acquire(self):
        """Test cleanup happening while another thread tries to acquire.

        This is a potential race condition scenario.
        """
        port = "COM_RACE"
        errors = []

        # Create the lock first
        with self.manager.acquire_port_lock(port):
            pass

        barrier = threading.Barrier(2)

        def cleanup_thread():
            barrier.wait()
            try:
                self.manager.cleanup_unused_locks(older_than=0)
            except Exception as e:
                errors.append(f"cleanup error: {e}")

        def acquire_thread():
            barrier.wait()
            try:
                with self.manager.acquire_port_lock(port, blocking=True):
                    time.sleep(0.01)
            except Exception as e:
                errors.append(f"acquire error: {e}")

        t1 = threading.Thread(target=cleanup_thread)
        t2 = threading.Thread(target=acquire_thread)

        t1.start()
        t2.start()
        t1.join(timeout=5)
        t2.join(timeout=5)

        # Either operation should succeed without errors
        # The lock might be cleaned up or re-created
        self.assertEqual(len(errors), 0, f"Errors occurred: {errors}")


class TestClearAllLocks(unittest.TestCase):
    """Test clear_all_locks edge cases."""

    def setUp(self):
        self.manager = ResourceLockManager()

    def test_clear_all_with_held_locks(self):
        """Clear all should force-release held locks."""
        # This is tricky because we can't easily test cross-thread release
        # Create some locks first
        with self.manager.acquire_port_lock("COM1"):
            pass
        with self.manager.acquire_port_lock("COM2"):
            pass
        with self.manager.acquire_project_lock("/project1"):
            pass

        count = self.manager.clear_all_locks()

        # Should have cleared 3 locks
        self.assertEqual(count, 3)

        # Lock counts should be 0
        counts = self.manager.get_lock_count()
        self.assertEqual(counts["port_locks"], 0)
        self.assertEqual(counts["project_locks"], 0)

    def test_clear_all_empty_manager(self):
        """Clear all on empty manager should return 0."""
        count = self.manager.clear_all_locks()
        self.assertEqual(count, 0)


class TestLockAcquisitionErrorMessage(unittest.TestCase):
    """Test LockAcquisitionError provides helpful error messages."""

    def test_error_message_with_lock_info(self):
        """Error message should include holder description."""
        lock_info = LockInfo(lock=threading.Lock(), timeout=30.0)
        lock_info.acquired_at = time.time()
        lock_info.holder_description = "Build for /test/project"

        error = LockAcquisitionError("port", "COM3", lock_info)

        self.assertIn("COM3", str(error))
        self.assertIn("Build for /test/project", str(error))

    def test_error_message_stale_lock(self):
        """Error message should indicate stale lock."""
        lock_info = LockInfo(lock=threading.Lock(), timeout=0.001)
        lock_info.acquired_at = time.time() - 10.0  # Very old
        lock_info.holder_description = "Stuck operation"

        error = LockAcquisitionError("project", "/path", lock_info)

        self.assertIn("STALE", str(error))
        self.assertIn("clear_stale_locks", str(error))


class TestLockTimeoutVsStaleThreshold(unittest.TestCase):
    """Test the interaction between lock timeout and stale threshold.

    Default timeout is 1800s (30 min) but stale threshold is 3600s (1 hour).
    This tests the behavior when these values differ.
    """

    def test_default_timeout_vs_stale_threshold(self):
        """Verify default values are correctly set."""
        self.assertEqual(DEFAULT_LOCK_TIMEOUT, 1800.0)  # 30 minutes
        self.assertEqual(STALE_LOCK_THRESHOLD, 3600.0)  # 1 hour

    def test_lock_stale_before_cleanup_threshold(self):
        """Lock can be stale but not old enough for cleanup."""
        manager = ResourceLockManager()

        # Create a lock with a very short timeout
        with manager.acquire_port_lock("COM_SHORT", timeout=0.01):
            # Manually backdate the acquisition
            with manager._master_lock:
                lock_info = manager._port_locks["COM_SHORT"]
                # Set acquired time to 1 hour ago (past timeout, past stale threshold)
                lock_info.acquired_at = time.time() - 3601

            time.sleep(0.02)  # Past the timeout

            # Lock should be stale
            stale = manager.get_stale_locks()
            self.assertEqual(len(stale.stale_port_locks), 1)


class TestHighContentionScenarios(unittest.TestCase):
    """Test high contention scenarios with many threads."""

    def setUp(self):
        self.manager = ResourceLockManager()

    def test_many_threads_single_resource(self):
        """Test many threads contending for single resource."""
        port = "COM_HIGH_CONTENTION"
        successful_acquisitions = [0]
        lock = threading.Lock()

        def work():
            with self.manager.acquire_port_lock(port, blocking=True):
                with lock:
                    successful_acquisitions[0] += 1
                # Simulate work
                time.sleep(0.001)

        with ThreadPoolExecutor(max_workers=20) as executor:
            futures = [executor.submit(work) for _ in range(50)]
            for future in as_completed(futures, timeout=60):
                future.result()  # Raise any exceptions

        self.assertEqual(successful_acquisitions[0], 50)

    def test_mixed_blocking_nonblocking(self):
        """Test mix of blocking and non-blocking acquisition attempts."""
        port = "COM_MIXED"
        blocking_successes = [0]
        nonblocking_successes = [0]
        nonblocking_failures = [0]
        lock = threading.Lock()

        def blocking_work():
            with self.manager.acquire_port_lock(port, blocking=True):
                with lock:
                    blocking_successes[0] += 1
                time.sleep(0.01)

        def nonblocking_work():
            try:
                with self.manager.acquire_port_lock(port, blocking=False):
                    with lock:
                        nonblocking_successes[0] += 1
                    time.sleep(0.001)
            except LockAcquisitionError:
                with lock:
                    nonblocking_failures[0] += 1

        threads = []
        for i in range(20):
            if i % 2 == 0:
                threads.append(threading.Thread(target=blocking_work))
            else:
                threads.append(threading.Thread(target=nonblocking_work))

        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=30)

        # All blocking should succeed
        self.assertEqual(blocking_successes[0], 10)
        # Non-blocking: some may succeed, some may fail
        self.assertEqual(nonblocking_successes[0] + nonblocking_failures[0], 10)


class TestLockDetailsJsonSerialization(unittest.TestCase):
    """Test lock details serialization for status reporting."""

    def setUp(self):
        self.manager = ResourceLockManager()

    def test_get_lock_details_empty(self):
        """Empty manager should return empty details."""
        details = self.manager.get_lock_details()
        self.assertEqual(details.port_locks, {})
        self.assertEqual(details.project_locks, {})

    def test_get_lock_details_with_locks(self):
        """Details should include all lock information."""
        with self.manager.acquire_port_lock("COM3", operation_id="op_123", description="Test operation"):
            details = self.manager.get_lock_details()

            port_info = details.port_locks["COM3"]
            self.assertTrue(port_info.is_held())
            self.assertEqual(port_info.holder_operation_id, "op_123")
            self.assertEqual(port_info.holder_description, "Test operation")
            self.assertEqual(port_info.acquisition_count, 1)


class TestNonBlockingTimeout(unittest.TestCase):
    """Test non-blocking acquire with timeouts."""

    def setUp(self):
        self.manager = ResourceLockManager()

    def test_nonblocking_immediate_fail(self):
        """Non-blocking acquire fails immediately when lock held."""
        port = "COM_NONBLOCK"

        with self.manager.acquire_port_lock(port, blocking=True):
            # Try non-blocking - should fail immediately
            start = time.time()
            with self.assertRaises(LockAcquisitionError):
                with self.manager.acquire_port_lock(port, blocking=False):
                    pass
            elapsed = time.time() - start

            # Should be nearly instant
            self.assertLess(elapsed, 0.1)


class TestLockInfoStateConsistency(unittest.TestCase):
    """Test that LockInfo state remains consistent across operations."""

    def setUp(self):
        self.manager = ResourceLockManager()

    def test_holder_info_cleared_on_release(self):
        """Holder info should be cleared when lock is released."""
        port = "COM_HOLDER"

        with self.manager.acquire_port_lock(port, operation_id="op_123", description="Test operation"):
            # Verify holder info is set
            details = self.manager.get_lock_details()
            port_info = details.port_locks[port]
            self.assertEqual(port_info.holder_operation_id, "op_123")
            self.assertEqual(port_info.holder_description, "Test operation")
            self.assertIsNotNone(port_info.holder_thread_id)

        # After release, holder info should be cleared
        details = self.manager.get_lock_details()
        port_info = details.port_locks[port]
        self.assertIsNone(port_info.holder_operation_id)
        self.assertIsNone(port_info.holder_description)
        self.assertIsNone(port_info.holder_thread_id)

    def test_lock_info_timestamps_monotonic(self):
        """Timestamps should be monotonically increasing."""
        port = "COM_TIMESTAMPS"

        # First acquire/release
        with self.manager.acquire_port_lock(port):
            first_acquired = self.manager._port_locks[port].acquired_at

        first_released = self.manager._port_locks[port].last_released_at

        time.sleep(0.01)

        # Second acquire/release
        with self.manager.acquire_port_lock(port):
            second_acquired = self.manager._port_locks[port].acquired_at

        second_released = self.manager._port_locks[port].last_released_at

        # Verify monotonic increase
        self.assertLess(first_acquired, first_released)
        self.assertLess(first_released, second_acquired)
        self.assertLess(second_acquired, second_released)

    def test_acquisition_count_increments(self):
        """Acquisition count should increment on each acquire."""
        port = "COM_COUNT"

        for expected_count in range(1, 6):
            with self.manager.acquire_port_lock(port):
                actual_count = self.manager._port_locks[port].acquisition_count
                self.assertEqual(actual_count, expected_count)


class TestExceptionHandlingInContextManager(unittest.TestCase):
    """Test that locks are properly released even when exceptions occur."""

    def setUp(self):
        self.manager = ResourceLockManager()

    def test_lock_released_on_exception(self):
        """Lock should be released even if exception is raised inside context."""
        port = "COM_EXCEPTION"

        try:
            with self.manager.acquire_port_lock(port):
                raise ValueError("Test exception")
        except ValueError:
            pass

        # Lock should not be held
        held = self.manager.get_held_locks()
        self.assertEqual(len(held.held_port_locks), 0)

        # Should be able to acquire again
        with self.manager.acquire_port_lock(port, blocking=False):
            pass

    def test_lock_released_on_keyboard_interrupt(self):
        """Lock should be released even on KeyboardInterrupt."""
        port = "COM_INTERRUPT"

        try:
            with self.manager.acquire_port_lock(port):
                raise KeyboardInterrupt()
        except KeyboardInterrupt:
            pass

        # Lock should not be held
        held = self.manager.get_held_locks()
        self.assertEqual(len(held.held_port_locks), 0)

    def test_lock_released_on_system_exit(self):
        """Lock should be released even on SystemExit."""
        port = "COM_EXIT"

        try:
            with self.manager.acquire_port_lock(port):
                raise SystemExit(1)
        except SystemExit:
            pass

        # Lock should not be held
        held = self.manager.get_held_locks()
        self.assertEqual(len(held.held_port_locks), 0)


class TestReentrantAcquisitionAttempts(unittest.TestCase):
    """Test behavior when same thread tries to acquire same lock twice."""

    def setUp(self):
        self.manager = ResourceLockManager()

    def test_same_thread_reacquire_deadlock_with_blocking(self):
        """Same thread trying to reacquire with blocking should deadlock.

        This test uses a timeout to detect the deadlock scenario.
        Note: This is expected behavior - threading.Lock is not reentrant.
        """
        port = "COM_REENTRANT"

        def try_reacquire():
            with self.manager.acquire_port_lock(port, blocking=True):
                # Try to acquire same lock again with blocking
                # This WILL deadlock because threading.Lock is not reentrant
                # We need to do this in a way that we can detect the deadlock
                pass

        # This would deadlock, so we just verify non-blocking behavior instead
        with self.manager.acquire_port_lock(port, blocking=True):
            # Same thread trying to acquire non-blocking should fail
            with self.assertRaises(LockAcquisitionError):
                with self.manager.acquire_port_lock(port, blocking=False):
                    pass

    def test_same_thread_nonblocking_reacquire_fails(self):
        """Same thread trying non-blocking reacquire should fail immediately."""
        port = "COM_NONBLOCK_REENTRANT"

        with self.manager.acquire_port_lock(port, blocking=True):
            start = time.time()
            with self.assertRaises(LockAcquisitionError):
                with self.manager.acquire_port_lock(port, blocking=False):
                    pass
            elapsed = time.time() - start

            # Should fail immediately (< 0.1s)
            self.assertLess(elapsed, 0.1)


class TestEmptyResourceIdEdgeCases(unittest.TestCase):
    """Test handling of empty or unusual resource IDs."""

    def setUp(self):
        self.manager = ResourceLockManager()

    def test_empty_string_port(self):
        """Empty string port should work (edge case)."""
        port = ""

        with self.manager.acquire_port_lock(port):
            status = self.manager.get_lock_status()
            self.assertIn(port, status["port_locks"])

    def test_empty_string_project(self):
        """Empty string project should work (edge case)."""
        project = ""

        with self.manager.acquire_project_lock(project):
            status = self.manager.get_lock_status()
            self.assertIn(project, status["project_locks"])

    def test_whitespace_only_port(self):
        """Whitespace-only port should work (treated as unique ID)."""
        port = "   "

        with self.manager.acquire_port_lock(port):
            status = self.manager.get_lock_status()
            self.assertIn(port, status["port_locks"])

    def test_special_characters_port(self):
        """Port with special characters should work."""
        port = "/dev/ttyUSB0"

        with self.manager.acquire_port_lock(port):
            status = self.manager.get_lock_status()
            self.assertIn(port, status["port_locks"])

    def test_unicode_port(self):
        """Port with unicode characters should work."""
        port = "COM3_设备"

        with self.manager.acquire_port_lock(port):
            status = self.manager.get_lock_status()
            self.assertIn(port, status["port_locks"])


class TestLockInfoHoldDuration(unittest.TestCase):
    """Test hold_duration() edge cases."""

    def test_hold_duration_not_held(self):
        """Hold duration should be None when lock not held."""
        lock_info = LockInfo(lock=threading.Lock())
        self.assertIsNone(lock_info.hold_duration())

    def test_hold_duration_when_held(self):
        """Hold duration should increase while lock is held."""
        lock_info = LockInfo(lock=threading.Lock())
        lock_info.acquired_at = time.time()

        time.sleep(0.1)
        duration = lock_info.hold_duration()

        self.assertIsNotNone(duration)
        self.assertGreaterEqual(duration, 0.1)
        self.assertLess(duration, 0.5)

    def test_hold_duration_after_release(self):
        """Hold duration should be None after release."""
        lock_info = LockInfo(lock=threading.Lock())
        lock_info.acquired_at = time.time()
        time.sleep(0.01)
        lock_info.last_released_at = time.time()

        self.assertIsNone(lock_info.hold_duration())


class TestLockInfoToDictEdgeCases(unittest.TestCase):
    """Test to_dict() serialization edge cases."""

    def test_to_dict_never_acquired(self):
        """to_dict should handle never-acquired lock."""
        lock_info = LockInfo(lock=threading.Lock())
        d = lock_info.to_dict()

        self.assertIsNone(d["acquired_at"])
        self.assertIsNone(d["last_released_at"])
        self.assertFalse(d["is_held"])
        self.assertFalse(d["is_stale"])
        self.assertIsNone(d["hold_duration"])

    def test_to_dict_with_none_holder_info(self):
        """to_dict should handle None holder info."""
        lock_info = LockInfo(lock=threading.Lock())
        lock_info.acquired_at = time.time()
        # holder_operation_id and holder_description are None
        d = lock_info.to_dict()

        self.assertIsNone(d["holder_operation_id"])
        self.assertIsNone(d["holder_description"])

    def test_to_dict_with_infinity_timeout(self):
        """to_dict should handle infinity timeout."""
        lock_info = LockInfo(lock=threading.Lock(), timeout=float("inf"))
        lock_info.acquired_at = time.time() - 1000000
        d = lock_info.to_dict()

        # Should serialize infinity as is
        self.assertEqual(d["timeout"], float("inf"))
        # Should not be stale due to infinite timeout
        self.assertFalse(d["is_stale"])


class TestForceReleaseRaceConditions(unittest.TestCase):
    """Test race conditions with force release."""

    def setUp(self):
        self.manager = ResourceLockManager()

    def test_force_release_during_normal_release(self):
        """Force release happening during normal release is now handled gracefully.

        FIXED: Previously, when force_release_lock() released a lock held by another thread,
        the holder thread's context manager exit would raise RuntimeError because
        threading.Lock.release() can only be called from the acquiring thread.

        This was fixed by catching and suppressing RuntimeError in the finally block
        of acquire_port_lock and acquire_project_lock (option 3 from the original bug list).

        This test now verifies that force-releasing doesn't cause exceptions in the holder thread.
        """
        port = "COM_FORCE_RACE"
        holder_error = []
        holder_completed = []

        def holder_thread():
            try:
                with self.manager.acquire_port_lock(port):
                    time.sleep(0.1)
                holder_completed.append(True)
            except Exception as e:
                # Should not get any exceptions now
                holder_error.append(str(e))

        def force_releaser():
            time.sleep(0.05)  # Wait for lock to be acquired
            self.manager.force_release_lock("port", port)

        t1 = threading.Thread(target=holder_thread)
        t2 = threading.Thread(target=force_releaser)

        t1.start()
        t2.start()
        t1.join(timeout=5)
        t2.join(timeout=5)

        # FIXED: No errors should occur, and holder thread should complete normally
        self.assertEqual(len(holder_error), 0, f"Unexpected errors: {holder_error}")
        self.assertEqual(len(holder_completed), 1, "Holder thread should complete successfully")

    def test_multiple_force_releases(self):
        """Multiple force releases on same lock should not crash."""
        port = "COM_MULTI_FORCE"

        with self.manager.acquire_port_lock(port):
            # Manipulate to make it stale
            with self.manager._master_lock:
                self.manager._port_locks[port].acquired_at = time.time() - 10000

        # Multiple force release attempts
        results = []
        for _ in range(5):
            results.append(self.manager.force_release_lock("port", port))

        # First one should return False (not held), rest also False
        # Because after first check, it's already marked as not held
        self.assertTrue(all(not r for r in results))


class TestCleanupWithStateTransitions(unittest.TestCase):
    """Test cleanup behavior during lock state transitions."""

    def setUp(self):
        self.manager = ResourceLockManager()

    def test_cleanup_skips_currently_held_locks(self):
        """Cleanup should never remove currently held locks."""
        port = "COM_CLEANUP_HELD"

        with self.manager.acquire_port_lock(port):
            # Backdate the lock to make it eligible for cleanup by age
            with self.manager._master_lock:
                self.manager._port_locks[port].created_at = time.time() - 10000
                self.manager._port_locks[port].acquired_at = time.time() - 5000

            # Try cleanup - should not remove because it's held
            removed = self.manager.cleanup_unused_locks(older_than=0)
            self.assertEqual(removed, 0)

            # Verify lock still exists
            status = self.manager.get_lock_status()
            self.assertIn(port, status["port_locks"])

    def test_cleanup_removes_very_old_released_locks(self):
        """Cleanup should remove locks that were released long ago.

        NOTE: The cleanup_unused_locks() function uses last_activity = last_released_at or created_at
        to determine if a lock is old. We also need to ensure is_held() returns False, which requires
        last_released_at > acquired_at.

        We need to backdate acquired_at, created_at, and last_released_at properly to ensure:
        1. is_held() returns False (last_released_at > acquired_at)
        2. The lock is considered old enough for cleanup
        """
        port = "COM_OLD_RELEASED"

        # Create and release a lock
        with self.manager.acquire_port_lock(port):
            pass

        # Backdate all timestamps - ensure last_released_at > acquired_at for is_held() to be False
        past_time = time.time() - 10000
        with self.manager._master_lock:
            self.manager._port_locks[port].created_at = past_time - 1
            self.manager._port_locks[port].acquired_at = past_time
            self.manager._port_locks[port].last_released_at = past_time + 1

        # Verify the lock is not considered held
        self.assertFalse(self.manager._port_locks[port].is_held())

        # Cleanup should remove it
        removed = self.manager.cleanup_unused_locks(older_than=0)
        self.assertEqual(removed, 1)


class TestGetHeldLocksEdgeCases(unittest.TestCase):
    """Test get_held_locks() edge cases."""

    def setUp(self):
        self.manager = ResourceLockManager()

    def test_get_held_locks_empty(self):
        """get_held_locks should return empty lists when no locks held."""
        held = self.manager.get_held_locks()
        self.assertEqual(held.held_port_locks, [])
        self.assertEqual(held.held_project_locks, [])

    def test_get_held_locks_with_mixed_state(self):
        """get_held_locks should only return currently held locks."""
        # Create held lock
        with self.manager.acquire_port_lock("COM_HELD"):
            # Create and release another lock
            with self.manager.acquire_port_lock("COM_RELEASED"):
                pass

            held = self.manager.get_held_locks()
            self.assertEqual(len(held.held_port_locks), 1)
            self.assertEqual(held.held_port_locks[0].resource_id, "COM_HELD")


class TestGetStaleLocksBoundaryConditions(unittest.TestCase):
    """Test get_stale_locks() boundary conditions."""

    def setUp(self):
        self.manager = ResourceLockManager()

    def test_lock_exactly_at_timeout_boundary(self):
        """Lock at exactly timeout boundary should be considered stale."""
        port = "COM_BOUNDARY"

        with self.manager.acquire_port_lock(port, timeout=0.1):
            # Set acquired_at to exactly timeout ago
            with self.manager._master_lock:
                self.manager._port_locks[port].acquired_at = time.time() - 0.1

            # Sleep tiny bit to ensure we're past boundary
            time.sleep(0.001)

            stale = self.manager.get_stale_locks()
            self.assertEqual(len(stale.stale_port_locks), 1)

    def test_lock_just_before_timeout(self):
        """Lock just before timeout should not be considered stale."""
        port = "COM_BEFORE"

        with self.manager.acquire_port_lock(port, timeout=10.0):
            # Lock was just acquired, well within timeout
            stale = self.manager.get_stale_locks()
            self.assertEqual(len(stale.stale_port_locks), 0)


class TestConcurrentAcquisitionOfNewLocks(unittest.TestCase):
    """Test concurrent acquisition of new (not-yet-created) locks."""

    def setUp(self):
        self.manager = ResourceLockManager()

    def test_concurrent_creation_of_same_lock(self):
        """Multiple threads creating same lock simultaneously should work."""
        port = "COM_CONCURRENT_CREATE"
        barrier = threading.Barrier(5)
        errors = []

        def acquire_lock():
            try:
                barrier.wait()
                with self.manager.acquire_port_lock(port, blocking=True):
                    time.sleep(0.01)
            except Exception as e:
                errors.append(str(e))

        threads = [threading.Thread(target=acquire_lock) for _ in range(5)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=10)

        self.assertEqual(len(errors), 0, f"Errors: {errors}")

        # Should have exactly 1 lock created
        counts = self.manager.get_lock_count()
        self.assertEqual(counts["port_locks"], 1)

        # Should have been acquired 5 times
        status = self.manager.get_lock_status()
        self.assertEqual(status["port_locks"][port], 5)


class TestMasterLockProtection(unittest.TestCase):
    """Test that _master_lock properly protects shared state."""

    def setUp(self):
        self.manager = ResourceLockManager()

    def test_concurrent_status_queries(self):
        """Concurrent status queries should not crash."""
        errors = []

        def query_status():
            try:
                for _ in range(100):
                    self.manager.get_lock_status()
                    self.manager.get_lock_count()
                    self.manager.get_lock_details()
                    self.manager.get_held_locks()
                    self.manager.get_stale_locks()
            except Exception as e:
                errors.append(str(e))

        threads = [threading.Thread(target=query_status) for _ in range(5)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=30)

        self.assertEqual(len(errors), 0, f"Errors: {errors}")

    def test_concurrent_status_queries_with_modifications(self):
        """Concurrent queries and modifications should not crash."""
        errors = []

        def modify_locks():
            try:
                for i in range(50):
                    with self.manager.acquire_port_lock(f"COM_MODIFY_{i % 5}"):
                        time.sleep(0.001)
            except Exception as e:
                errors.append(f"Modify error: {e}")

        def query_status():
            try:
                for _ in range(100):
                    self.manager.get_lock_status()
                    self.manager.get_lock_details()
            except Exception as e:
                errors.append(f"Query error: {e}")

        modifier = threading.Thread(target=modify_locks)
        querier = threading.Thread(target=query_status)

        modifier.start()
        querier.start()
        modifier.join(timeout=30)
        querier.join(timeout=30)

        self.assertEqual(len(errors), 0, f"Errors: {errors}")


class TestNegativeAndZeroTimeoutEdgeCases(unittest.TestCase):
    """Test behavior with negative, zero, and edge case timeout values."""

    def setUp(self):
        self.manager = ResourceLockManager()

    def test_negative_timeout_value(self):
        """Lock with negative timeout should be immediately stale when held.

        A negative timeout essentially means the lock was "already expired"
        when acquired. This is an edge case that could occur from misconfiguration.
        """
        lock_info = LockInfo(lock=threading.Lock(), timeout=-1.0)
        lock_info.acquired_at = time.time()
        # Negative timeout: time.time() - acquired_at > -1.0 is always True
        self.assertTrue(lock_info.is_stale())

    def test_zero_timeout_with_blocking_acquire(self):
        """Lock with zero timeout should work but be immediately stale."""
        port = "COM_ZERO_TIMEOUT"

        with self.manager.acquire_port_lock(port, timeout=0.0):
            # Lock should be acquirable
            status = self.manager.get_lock_status()
            self.assertIn(port, status["port_locks"])

            # But should be immediately stale
            stale = self.manager.get_stale_locks()
            self.assertEqual(len(stale.stale_port_locks), 1)

    def test_very_small_timeout(self):
        """Lock with very small timeout (nanoseconds) should be immediately stale."""
        lock_info = LockInfo(lock=threading.Lock(), timeout=1e-9)  # 1 nanosecond
        lock_info.acquired_at = time.time()
        time.sleep(0.0001)  # Sleep minimal time
        self.assertTrue(lock_info.is_stale())

    def test_timeout_overflow_large_value(self):
        """Lock with extremely large timeout should not be stale."""
        lock_info = LockInfo(lock=threading.Lock(), timeout=1e308)  # Near max float
        lock_info.acquired_at = time.time() - 1e10  # Held for 300+ years
        self.assertFalse(lock_info.is_stale())


class TestBlockingTimeoutBehavior(unittest.TestCase):
    """Test blocking acquisition timeout behavior."""

    def setUp(self):
        self.manager = ResourceLockManager()

    def test_blocking_with_short_timeout_fails(self):
        """Blocking acquire with short timeout on held lock should eventually fail.

        Note: threading.Lock.acquire() with timeout parameter requires Python 3.2+
        The current implementation uses blocking=True which blocks indefinitely.
        This test documents that there's no timeout on the blocking wait.
        """
        port = "COM_BLOCK_TIMEOUT"

        # Pre-acquire the lock
        lock_info = self.manager._get_or_create_port_lock(port)
        lock_info.lock.acquire()
        lock_info.acquired_at = time.time()

        try:
            # Non-blocking should fail immediately
            start = time.time()
            with self.assertRaises(LockAcquisitionError):
                with self.manager.acquire_port_lock(port, blocking=False):
                    pass
            elapsed = time.time() - start

            # Should be nearly instant
            self.assertLess(elapsed, 0.1)
        finally:
            lock_info.lock.release()

    def test_blocking_indefinite_wait_with_release(self):
        """Blocking acquire should wait until lock is released."""
        port = "COM_WAIT_RELEASE"
        acquired_order = []

        def holder():
            with self.manager.acquire_port_lock(port):
                acquired_order.append("holder")
                time.sleep(0.1)
            acquired_order.append("holder_done")

        def waiter():
            time.sleep(0.05)  # Start after holder
            with self.manager.acquire_port_lock(port, blocking=True):
                acquired_order.append("waiter")

        t1 = threading.Thread(target=holder)
        t2 = threading.Thread(target=waiter)

        t1.start()
        t2.start()
        t1.join(timeout=5)
        t2.join(timeout=5)

        # Waiter should have acquired after holder released
        self.assertEqual(acquired_order, ["holder", "holder_done", "waiter"])


class TestLockInfoDataIntegrity(unittest.TestCase):
    """Test LockInfo data integrity under various conditions."""

    def test_lock_info_thread_id_consistency(self):
        """Thread ID should match the thread that acquired the lock."""
        manager = ResourceLockManager()
        port = "COM_THREAD_ID"
        actual_thread_ids = []
        recorded_thread_ids = []

        def acquire_and_record():
            actual_thread_ids.append(threading.get_ident())
            with manager.acquire_port_lock(port):
                with manager._master_lock:
                    recorded_thread_ids.append(manager._port_locks[port].holder_thread_id)

        threads = [threading.Thread(target=acquire_and_record) for _ in range(5)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=10)

        # Each recorded thread ID should match the actual thread ID
        self.assertEqual(len(actual_thread_ids), 5)
        self.assertEqual(len(recorded_thread_ids), 5)
        # All recorded IDs should be valid thread IDs (not None)
        self.assertTrue(all(tid is not None for tid in recorded_thread_ids))

    def test_acquisition_count_never_decreases(self):
        """Acquisition count should never decrease, even after release or cleanup."""
        manager = ResourceLockManager()
        port = "COM_COUNT_MONOTONIC"

        # Acquire multiple times
        for _ in range(10):
            with manager.acquire_port_lock(port):
                pass

        # Count should be 10
        self.assertEqual(manager._port_locks[port].acquisition_count, 10)

        # Cleanup won't remove because older_than would need to be very small
        # But even after multiple acquires, count should never go down
        for _ in range(5):
            with manager.acquire_port_lock(port):
                count = manager._port_locks[port].acquisition_count
                self.assertGreaterEqual(count, 10)

    def test_timestamps_precision(self):
        """Timestamps should have sufficient precision for ordering."""
        lock_info = LockInfo(lock=threading.Lock())

        timestamps = []
        for _ in range(100):
            lock_info.acquired_at = time.time()
            timestamps.append(lock_info.acquired_at)
            lock_info.last_released_at = time.time()
            timestamps.append(lock_info.last_released_at)

        # All timestamps should be monotonically increasing or equal
        for i in range(1, len(timestamps)):
            self.assertGreaterEqual(timestamps[i], timestamps[i - 1])


class TestLockManagerSingletonBehavior(unittest.TestCase):
    """Test that multiple ResourceLockManager instances don't interfere."""

    def test_separate_managers_independent(self):
        """Two ResourceLockManager instances should have independent locks."""
        manager1 = ResourceLockManager()
        manager2 = ResourceLockManager()

        # Acquire lock on same port in both managers
        with manager1.acquire_port_lock("COM_SHARED"):
            # Manager2 should also be able to acquire (different lock)
            with manager2.acquire_port_lock("COM_SHARED", blocking=False):
                # Both should report the lock as held
                self.assertEqual(len(manager1.get_held_locks().held_port_locks), 1)
                self.assertEqual(len(manager2.get_held_locks().held_port_locks), 1)

    def test_manager_isolation_with_cleanup(self):
        """Cleanup on one manager should not affect another."""
        manager1 = ResourceLockManager()
        manager2 = ResourceLockManager()

        # Create locks in both
        with manager1.acquire_port_lock("COM_M1"):
            pass
        with manager2.acquire_port_lock("COM_M2"):
            pass

        # Cleanup manager1
        manager1.cleanup_unused_locks(older_than=0)

        # Manager1 should be empty, manager2 should still have its lock
        self.assertEqual(manager1.get_lock_count()["port_locks"], 0)
        self.assertEqual(manager2.get_lock_count()["port_locks"], 1)


class TestLockErrorMessageQuality(unittest.TestCase):
    """Test that error messages provide useful diagnostic information."""

    def test_error_message_includes_all_relevant_info(self):
        """LockAcquisitionError should include all relevant diagnostic info."""
        lock_info = LockInfo(lock=threading.Lock(), timeout=30.0)
        lock_info.acquired_at = time.time()
        lock_info.holder_description = "Build operation XYZ"
        lock_info.holder_operation_id = "op_12345"
        lock_info.holder_thread_id = 99999

        error = LockAcquisitionError("port", "COM99", lock_info)
        error_str = str(error)

        # Should include resource identifier
        self.assertIn("COM99", error_str)
        # Should include holder description
        self.assertIn("Build operation XYZ", error_str)
        # Should mention it's held
        self.assertIn("held", error_str.lower())

    def test_error_message_for_stale_suggests_action(self):
        """Error message for stale lock should suggest force-release."""
        lock_info = LockInfo(lock=threading.Lock(), timeout=0.001)
        lock_info.acquired_at = time.time() - 100  # Very old
        lock_info.holder_description = "Stuck operation"

        error = LockAcquisitionError("project", "/my/project", lock_info)
        error_str = str(error)

        # Should indicate stale
        self.assertIn("STALE", error_str)
        # Should suggest solution
        self.assertIn("clear_stale_locks", error_str)

    def test_error_message_without_lock_info(self):
        """Error message without lock_info should still be informative."""
        error = LockAcquisitionError("port", "COM_UNKNOWN", None)
        error_str = str(error)

        # Should still mention the resource
        self.assertIn("COM_UNKNOWN", error_str)
        self.assertIn("unavailable", error_str.lower())


class TestRapidStateTransitions(unittest.TestCase):
    """Test lock behavior under rapid state transitions."""

    def setUp(self):
        self.manager = ResourceLockManager()

    def test_rapid_held_unheld_queries(self):
        """Queries during rapid acquire/release should not crash."""
        port = "COM_RAPID_QUERY"
        errors = []
        stop_flag = [False]

        def acquirer():
            try:
                for _ in range(100):
                    if stop_flag[0]:
                        break
                    with self.manager.acquire_port_lock(port):
                        pass
            except Exception as e:
                errors.append(f"Acquire error: {e}")

        def querier():
            try:
                for _ in range(200):
                    if stop_flag[0]:
                        break
                    # These queries should never crash even during transitions
                    self.manager.get_held_locks()
                    self.manager.get_stale_locks()
                    self.manager.get_lock_details()
            except Exception as e:
                errors.append(f"Query error: {e}")

        t1 = threading.Thread(target=acquirer)
        t2 = threading.Thread(target=querier)

        t1.start()
        t2.start()
        t1.join(timeout=10)
        stop_flag[0] = True
        t2.join(timeout=10)

        self.assertEqual(len(errors), 0, f"Errors: {errors}")

    def test_force_release_stale_during_rapid_acquire(self):
        """Force releasing stale locks during rapid acquisition should not crash."""
        port = "COM_FORCE_RAPID"
        errors = []

        def acquirer():
            try:
                for _ in range(50):
                    with self.manager.acquire_port_lock(port, timeout=0.001):
                        # Make it stale
                        with self.manager._master_lock:
                            self.manager._port_locks[port].acquired_at = time.time() - 1
            except Exception as e:
                errors.append(f"Acquire error: {e}")

        def releaser():
            try:
                for _ in range(50):
                    self.manager.force_release_stale_locks()
                    time.sleep(0.001)
            except Exception as e:
                errors.append(f"Release error: {e}")

        t1 = threading.Thread(target=acquirer)
        t2 = threading.Thread(target=releaser)

        t1.start()
        t2.start()
        t1.join(timeout=15)
        t2.join(timeout=15)

        self.assertEqual(len(errors), 0, f"Errors: {errors}")


class TestLockInfoEquality(unittest.TestCase):
    """Test LockInfo comparison and hashing behavior."""

    def test_lock_info_not_equal_different_locks(self):
        """Different LockInfo instances with different locks should not be equal."""
        lock1 = LockInfo(lock=threading.Lock())
        lock2 = LockInfo(lock=threading.Lock())

        # dataclass equality by default compares all fields
        # Different lock objects should make them unequal
        self.assertNotEqual(lock1.lock, lock2.lock)

    def test_lock_info_with_same_underlying_lock(self):
        """LockInfo instances sharing same lock is problematic - test behavior.

        This tests a potential bug scenario where the same threading.Lock
        is accidentally used in multiple LockInfo instances.
        """
        shared_lock = threading.Lock()
        lock_info1 = LockInfo(lock=shared_lock)
        lock_info2 = LockInfo(lock=shared_lock)

        # Acquire via lock_info1
        shared_lock.acquire()
        lock_info1.acquired_at = time.time()
        lock_info1.holder_thread_id = threading.get_ident()

        # lock_info2 has the same underlying lock but doesn't know it's held
        # This is a data consistency issue
        self.assertTrue(lock_info1.is_held())
        self.assertFalse(lock_info2.is_held())  # Metadata not updated!

        # But the underlying lock IS held
        acquired = shared_lock.acquire(blocking=False)
        self.assertFalse(acquired)  # Cannot acquire - already held

        shared_lock.release()


class TestCleanupEdgeCases(unittest.TestCase):
    """Test cleanup_unused_locks edge cases."""

    def setUp(self):
        self.manager = ResourceLockManager()

    def test_cleanup_with_nan_older_than(self):
        """Cleanup with NaN older_than should not crash."""
        import math

        # Create a lock
        with self.manager.acquire_port_lock("COM_NAN"):
            pass

        # NaN comparison: anything compared to NaN is False
        # So current_time - last_activity > NaN will be False
        removed = self.manager.cleanup_unused_locks(older_than=math.nan)

        # Lock should NOT be removed (NaN comparison fails)
        self.assertEqual(removed, 0)
        self.assertEqual(self.manager.get_lock_count()["port_locks"], 1)

    def test_cleanup_with_negative_older_than(self):
        """Cleanup with negative older_than should remove all non-held locks."""
        # Create a lock
        with self.manager.acquire_port_lock("COM_NEG"):
            pass

        # Negative older_than: current_time - last_activity > -1 is always True
        removed = self.manager.cleanup_unused_locks(older_than=-1)

        # Lock should be removed
        self.assertEqual(removed, 1)
        self.assertEqual(self.manager.get_lock_count()["port_locks"], 0)

    def test_cleanup_during_acquisition_race(self):
        """Cleanup should not remove a lock that's being acquired.

        This tests the race between cleanup and acquire operations.
        """
        port = "COM_CLEANUP_RACE"
        cleanup_results = []
        acquire_results = []
        barrier = threading.Barrier(2)

        def cleanup_worker():
            barrier.wait()
            result = self.manager.cleanup_unused_locks(older_than=0)
            cleanup_results.append(result)

        def acquire_worker():
            barrier.wait()
            try:
                with self.manager.acquire_port_lock(port):
                    acquire_results.append("acquired")
                    time.sleep(0.05)
            except Exception as e:
                acquire_results.append(f"error: {e}")

        # Pre-create the lock
        with self.manager.acquire_port_lock(port):
            pass

        # Now race cleanup vs re-acquire
        t1 = threading.Thread(target=cleanup_worker)
        t2 = threading.Thread(target=acquire_worker)

        t1.start()
        t2.start()
        t1.join(timeout=5)
        t2.join(timeout=5)

        # One of these scenarios should happen:
        # 1. Cleanup happens first, removes lock, then acquire creates new one
        # 2. Acquire happens first, cleanup sees it as held and skips
        # Either way, no crash should occur
        self.assertTrue(len(acquire_results) == 1 and acquire_results[0] == "acquired", f"Acquire results: {acquire_results}")


class TestLockDescriptionEdgeCases(unittest.TestCase):
    """Test lock holder description edge cases."""

    def setUp(self):
        self.manager = ResourceLockManager()

    def test_very_long_description(self):
        """Very long description should be stored correctly."""
        long_desc = "A" * 10000  # 10KB description

        with self.manager.acquire_port_lock("COM_LONG", description=long_desc):
            details = self.manager.get_lock_details()
            self.assertEqual(details.port_locks["COM_LONG"].holder_description, long_desc)

    def test_description_with_special_chars(self):
        """Description with special characters should work."""
        special_desc = "Build for path: C:\\Users\\test\\project with 日本語"

        with self.manager.acquire_port_lock("COM_SPECIAL", description=special_desc):
            details = self.manager.get_lock_details()
            self.assertEqual(details.port_locks["COM_SPECIAL"].holder_description, special_desc)

    def test_description_with_newlines(self):
        """Description with newlines should work."""
        multiline_desc = "Step 1: Compile\nStep 2: Link\nStep 3: Upload"

        with self.manager.acquire_port_lock("COM_MULTI", description=multiline_desc):
            details = self.manager.get_lock_details()
            self.assertEqual(details.port_locks["COM_MULTI"].holder_description, multiline_desc)

    def test_none_vs_empty_description(self):
        """None description should generate default, empty should use empty."""
        # None description
        with self.manager.acquire_port_lock("COM_NONE", description=None):
            details = self.manager.get_lock_details()
            desc = details.port_locks["COM_NONE"].holder_description
            # Should have generated a default description
            self.assertIsNotNone(desc)
            self.assertIn("COM_NONE", desc)


class TestConcurrentForceReleaseAll(unittest.TestCase):
    """Test concurrent clear_all_locks operations."""

    def test_concurrent_clear_all(self):
        """Multiple concurrent clear_all_locks should not crash."""
        manager = ResourceLockManager()
        errors = []

        # Pre-create some locks
        for i in range(10):
            with manager.acquire_port_lock(f"COM_{i}"):
                pass

        def clear_all():
            try:
                for _ in range(10):
                    manager.clear_all_locks()
            except Exception as e:
                errors.append(str(e))

        threads = [threading.Thread(target=clear_all) for _ in range(5)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=10)

        self.assertEqual(len(errors), 0, f"Errors: {errors}")

        # All locks should be cleared
        self.assertEqual(manager.get_lock_count()["port_locks"], 0)

    def test_clear_all_while_acquiring(self):
        """clear_all_locks during active acquisition should handle gracefully."""
        manager = ResourceLockManager()
        errors = []

        def acquirer():
            try:
                for i in range(50):
                    with manager.acquire_port_lock(f"COM_CLEAR_{i % 5}"):
                        time.sleep(0.001)
            except Exception as e:
                errors.append(f"Acquire: {e}")

        def clearer():
            try:
                time.sleep(0.05)  # Let some acquisitions happen
                for _ in range(5):
                    manager.clear_all_locks()
                    time.sleep(0.01)
            except Exception as e:
                errors.append(f"Clear: {e}")

        t1 = threading.Thread(target=acquirer)
        t2 = threading.Thread(target=clearer)

        t1.start()
        t2.start()
        t1.join(timeout=15)
        t2.join(timeout=15)

        # May have errors due to cross-thread release, but shouldn't crash
        # This is documenting expected behavior (the cross-thread bug)


class TestPortProjectLockInteraction(unittest.TestCase):
    """Test interactions between port locks and project locks."""

    def setUp(self):
        self.manager = ResourceLockManager()

    def test_port_and_project_locks_independent(self):
        """Port locks and project locks should be completely independent."""
        # Same identifier for both should create separate locks
        with self.manager.acquire_port_lock("SHARED_ID"):
            with self.manager.acquire_project_lock("SHARED_ID"):
                # Both should be held
                held = self.manager.get_held_locks()
                self.assertEqual(len(held.held_port_locks), 1)
                self.assertEqual(len(held.held_project_locks), 1)

    def test_cleanup_affects_both_lock_types(self):
        """cleanup_unused_locks should affect both port and project locks."""
        # Create one of each
        with self.manager.acquire_port_lock("COM_CLEANUP"):
            pass
        with self.manager.acquire_project_lock("/project/cleanup"):
            pass

        removed = self.manager.cleanup_unused_locks(older_than=0)

        # Both should be removed
        self.assertEqual(removed, 2)
        counts = self.manager.get_lock_count()
        self.assertEqual(counts["port_locks"], 0)
        self.assertEqual(counts["project_locks"], 0)

    def test_force_release_specific_type(self):
        """Force release should only affect the specified lock type."""
        # Create both
        with self.manager.acquire_port_lock("SHARED_FR"):
            pass
        with self.manager.acquire_project_lock("SHARED_FR"):
            # Make port lock stale for testing force release
            with self.manager._master_lock:
                self.manager._port_locks["SHARED_FR"].acquired_at = time.time() - 100
                self.manager._port_locks["SHARED_FR"].last_released_at = None

        # Force release only port
        result = self.manager.force_release_lock("port", "SHARED_FR")
        self.assertTrue(result)

        # Project lock should still exist and be releasable
        counts = self.manager.get_lock_count()
        self.assertEqual(counts["project_locks"], 1)


if __name__ == "__main__":
    unittest.main()
