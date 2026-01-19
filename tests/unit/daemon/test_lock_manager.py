"""
Unit tests for ResourceLockManager.

Tests lock acquisition, release, cleanup, and contention scenarios.
"""

import threading
import time
import unittest

from fbuild.daemon.lock_manager import ResourceLockManager


class TestResourceLockManager(unittest.TestCase):
    """Test cases for ResourceLockManager."""

    def setUp(self):
        """Create a fresh lock manager for each test."""
        self.manager = ResourceLockManager()

    def test_acquire_and_release_port_lock(self):
        """Test basic port lock acquisition and release."""
        port = "COM3"

        # Acquire lock
        with self.manager.acquire_port_lock(port):
            # Lock should be held
            status = self.manager.get_lock_status()
            self.assertIn(port, status["port_locks"])
            self.assertEqual(status["port_locks"][port], 1)

        # After release, lock should still exist but have been acquired once
        status = self.manager.get_lock_status()
        self.assertIn(port, status["port_locks"])
        self.assertEqual(status["port_locks"][port], 1)

    def test_acquire_and_release_project_lock(self):
        """Test basic project lock acquisition and release."""
        project_dir = "/path/to/project"

        # Acquire lock
        with self.manager.acquire_project_lock(project_dir):
            # Lock should be held
            status = self.manager.get_lock_status()
            self.assertIn(project_dir, status["project_locks"])
            self.assertEqual(status["project_locks"][project_dir], 1)

        # After release, lock should still exist but have been acquired once
        status = self.manager.get_lock_status()
        self.assertIn(project_dir, status["project_locks"])
        self.assertEqual(status["project_locks"][project_dir], 1)

    def test_non_blocking_lock_contention(self):
        """Test that non-blocking lock acquisition fails when lock is held."""
        port = "COM3"

        # Acquire lock (blocking)
        with self.manager.acquire_port_lock(port):
            # Try to acquire same lock non-blocking (should fail)
            with self.assertRaises(RuntimeError):
                with self.manager.acquire_port_lock(port, blocking=False):
                    pass

    def test_multiple_acquisitions_same_resource(self):
        """Test that the same lock can be acquired multiple times sequentially."""
        port = "COM3"

        # Acquire and release 3 times
        for _ in range(3):
            with self.manager.acquire_port_lock(port):
                pass

        # Check acquisition count
        status = self.manager.get_lock_status()
        self.assertEqual(status["port_locks"][port], 3)

    def test_cleanup_unused_locks(self):
        """Test that unused locks are cleaned up."""
        port1 = "COM3"
        port2 = "COM4"

        # Create two locks
        with self.manager.acquire_port_lock(port1):
            pass

        # Wait a bit
        time.sleep(0.1)

        # Create another lock more recently
        with self.manager.acquire_port_lock(port2):
            pass

        # Cleanup locks older than 0.05 seconds (should remove port1)
        removed = self.manager.cleanup_unused_locks(older_than=0.05)

        # Should have removed 1 lock
        self.assertEqual(removed, 1)

        # port1 should be gone, port2 should remain
        status = self.manager.get_lock_status()
        self.assertNotIn(port1, status["port_locks"])
        self.assertIn(port2, status["port_locks"])

    def test_concurrent_access_different_resources(self):
        """Test that different resources can be locked concurrently."""
        results = []

        def lock_port(port, delay):
            with self.manager.acquire_port_lock(port):
                time.sleep(delay)
                results.append(port)

        # Start two threads with different ports
        t1 = threading.Thread(target=lock_port, args=("COM3", 0.1))
        t2 = threading.Thread(target=lock_port, args=("COM4", 0.1))

        start = time.time()
        t1.start()
        t2.start()
        t1.join()
        t2.join()
        elapsed = time.time() - start

        # Both threads should complete in parallel (~0.1s, not 0.2s)
        # Use 0.18s threshold to account for system variance on Windows
        self.assertLess(elapsed, 0.18)
        self.assertEqual(len(results), 2)

    def test_concurrent_access_same_resource(self):
        """Test that same resource is serialized across threads."""
        results = []
        lock = threading.Lock()

        def lock_port(port, delay, thread_id):
            with self.manager.acquire_port_lock(port):
                time.sleep(delay)
                with lock:
                    results.append(thread_id)

        # Start two threads with same port
        t1 = threading.Thread(target=lock_port, args=("COM3", 0.05, 1))
        t2 = threading.Thread(target=lock_port, args=("COM3", 0.05, 2))

        start = time.time()
        t1.start()
        t2.start()
        t1.join()
        t2.join()
        elapsed = time.time() - start

        # Threads should be serialized (~0.1s total)
        self.assertGreater(elapsed, 0.08)
        self.assertEqual(len(results), 2)

    def test_get_lock_count(self):
        """Test lock count tracking."""
        # Initially no locks
        counts = self.manager.get_lock_count()
        self.assertEqual(counts["port_locks"], 0)
        self.assertEqual(counts["project_locks"], 0)

        # Create some locks
        with self.manager.acquire_port_lock("COM3"):
            pass
        with self.manager.acquire_project_lock("/path/to/project"):
            pass

        # Should have 1 of each
        counts = self.manager.get_lock_count()
        self.assertEqual(counts["port_locks"], 1)
        self.assertEqual(counts["project_locks"], 1)


if __name__ == "__main__":
    unittest.main()
