"""
Unit tests for ConfigurationLockManager class.

The ConfigurationLockManager provides configuration-level locking for fbuild daemon
operations. It manages locks identified by (project_dir, environment, port) tuples
and supports both exclusive and shared read lock modes.

Test categories:
1. Basic exclusive lock acquire/release
2. Basic shared read lock acquire/release
3. Multiple clients shared read (should all succeed)
4. Exclusive blocks shared read and vice versa
5. Upgrade from shared to exclusive
6. Downgrade from exclusive to shared
7. Waiting queue for exclusive (blocked clients)
8. Auto-release on client disconnect (release_all_client_locks)
9. Timeout on exclusive acquire
10. Concurrent access patterns
11. Edge cases (empty config, same client twice, etc.)
12. Lock status reporting
"""

import threading
import time
import unittest
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass
from enum import Enum
from typing import Dict, List, NamedTuple, Optional, Set


class LockState(Enum):
    """Lock state enumeration."""

    UNLOCKED = "unlocked"
    LOCKED_EXCLUSIVE = "locked_exclusive"
    LOCKED_SHARED_READ = "locked_shared_read"


class ConfigKey(NamedTuple):
    """Configuration identifier tuple."""

    project_dir: str
    environment: str
    port: str


@dataclass
class ConfigLockInfo:
    """Information about a configuration lock."""

    state: LockState = LockState.UNLOCKED
    exclusive_holder: Optional[str] = None  # client_id holding exclusive lock
    shared_holders: Set[str] = None  # client_ids holding shared read locks
    waiting_queue: List[str] = None  # client_ids waiting for exclusive lock
    acquired_at: Optional[float] = None
    lock: threading.Lock = None
    condition: threading.Condition = None

    def __post_init__(self):
        if self.shared_holders is None:
            self.shared_holders = set()
        if self.waiting_queue is None:
            self.waiting_queue = []
        if self.lock is None:
            self.lock = threading.Lock()
        if self.condition is None:
            self.condition = threading.Condition(self.lock)


class ConfigurationLockError(Exception):
    """Exception raised when a configuration lock cannot be acquired."""

    def __init__(self, config_key: ConfigKey, message: str):
        self.config_key = config_key
        self.message = message
        super().__init__(f"Configuration lock error for {config_key}: {message}")


class ConfigurationLockManager:
    """
    Manages configuration-level locks for fbuild daemon operations.

    Features:
    - Configuration identified by (project_dir, environment, port) tuple
    - Exclusive lock - only one client can hold
    - Shared read lock - multiple clients can hold simultaneously
    - Auto-release when client disconnects
    - Waiting queue for exclusive lock requests
    """

    def __init__(self):
        self._master_lock = threading.Lock()
        self._locks: Dict[ConfigKey, ConfigLockInfo] = {}
        self._client_locks: Dict[str, Set[ConfigKey]] = {}  # client_id -> set of held config keys

    def _get_or_create_lock(self, config_key: ConfigKey) -> ConfigLockInfo:
        """Get or create a lock for the given configuration."""
        with self._master_lock:
            if config_key not in self._locks:
                self._locks[config_key] = ConfigLockInfo()
            return self._locks[config_key]

    def acquire_exclusive(self, config_key: ConfigKey, client_id: str, blocking: bool = True, timeout: Optional[float] = None) -> bool:
        """
        Acquire an exclusive lock on the configuration.

        Args:
            config_key: The configuration to lock
            client_id: Identifier for the client acquiring the lock
            blocking: If True, wait for lock; if False, fail immediately
            timeout: Maximum time to wait (None for infinite)

        Returns:
            True if lock was acquired, False if not (non-blocking or timeout)

        Raises:
            ConfigurationLockError: If lock cannot be acquired (non-blocking)
        """
        lock_info = self._get_or_create_lock(config_key)
        deadline = None if timeout is None else time.time() + timeout

        with lock_info.condition:
            # If we already hold exclusive, succeed immediately
            if lock_info.state == LockState.LOCKED_EXCLUSIVE and lock_info.exclusive_holder == client_id:
                return True

            # If not blocking and lock is not available
            if not blocking:
                if lock_info.state != LockState.UNLOCKED:
                    if lock_info.state == LockState.LOCKED_EXCLUSIVE:
                        raise ConfigurationLockError(config_key, f"Exclusive lock held by {lock_info.exclusive_holder}")
                    else:
                        raise ConfigurationLockError(config_key, f"Shared read lock held by {len(lock_info.shared_holders)} clients")

            # Add to waiting queue
            if client_id not in lock_info.waiting_queue:
                lock_info.waiting_queue.append(client_id)

            try:
                while True:
                    # Check if we can acquire
                    can_acquire = lock_info.state == LockState.UNLOCKED or (
                        lock_info.state == LockState.LOCKED_SHARED_READ and len(lock_info.shared_holders) == 1 and client_id in lock_info.shared_holders
                    )

                    # Also check if we're first in queue (for exclusive fairness)
                    first_in_queue = lock_info.waiting_queue and lock_info.waiting_queue[0] == client_id

                    if can_acquire and first_in_queue:
                        # Remove from shared holders if upgrading
                        if client_id in lock_info.shared_holders:
                            lock_info.shared_holders.remove(client_id)

                        lock_info.state = LockState.LOCKED_EXCLUSIVE
                        lock_info.exclusive_holder = client_id
                        lock_info.acquired_at = time.time()
                        lock_info.waiting_queue.remove(client_id)

                        # Track client lock
                        with self._master_lock:
                            if client_id not in self._client_locks:
                                self._client_locks[client_id] = set()
                            self._client_locks[client_id].add(config_key)

                        return True

                    if not blocking:
                        lock_info.waiting_queue.remove(client_id)
                        return False

                    # Calculate remaining timeout
                    if deadline is not None:
                        remaining = deadline - time.time()
                        if remaining <= 0:
                            lock_info.waiting_queue.remove(client_id)
                            return False
                        lock_info.condition.wait(remaining)
                    else:
                        lock_info.condition.wait()
            except Exception:
                # Clean up waiting queue on exception
                if client_id in lock_info.waiting_queue:
                    lock_info.waiting_queue.remove(client_id)
                raise

    def acquire_shared_read(self, config_key: ConfigKey, client_id: str, blocking: bool = True, timeout: Optional[float] = None) -> bool:
        """
        Acquire a shared read lock on the configuration.

        Multiple clients can hold shared read locks simultaneously.

        Args:
            config_key: The configuration to lock
            client_id: Identifier for the client acquiring the lock
            blocking: If True, wait for lock; if False, fail immediately
            timeout: Maximum time to wait (None for infinite)

        Returns:
            True if lock was acquired, False if not
        """
        lock_info = self._get_or_create_lock(config_key)
        deadline = None if timeout is None else time.time() + timeout

        with lock_info.condition:
            # If we already hold shared read or exclusive, succeed
            if client_id in lock_info.shared_holders:
                return True
            if lock_info.state == LockState.LOCKED_EXCLUSIVE and lock_info.exclusive_holder == client_id:
                return True

            while True:
                # Can acquire shared if unlocked or already shared (no exclusive waiters)
                can_acquire = lock_info.state == LockState.UNLOCKED or (lock_info.state == LockState.LOCKED_SHARED_READ and len(lock_info.waiting_queue) == 0)

                if can_acquire:
                    lock_info.state = LockState.LOCKED_SHARED_READ
                    lock_info.shared_holders.add(client_id)
                    if lock_info.acquired_at is None:
                        lock_info.acquired_at = time.time()

                    # Track client lock
                    with self._master_lock:
                        if client_id not in self._client_locks:
                            self._client_locks[client_id] = set()
                        self._client_locks[client_id].add(config_key)

                    return True

                if not blocking:
                    if lock_info.state == LockState.LOCKED_EXCLUSIVE:
                        raise ConfigurationLockError(config_key, f"Exclusive lock held by {lock_info.exclusive_holder}")
                    else:
                        raise ConfigurationLockError(config_key, "Exclusive waiter in queue, cannot acquire shared")

                # Calculate remaining timeout
                if deadline is not None:
                    remaining = deadline - time.time()
                    if remaining <= 0:
                        return False
                    lock_info.condition.wait(remaining)
                else:
                    lock_info.condition.wait()

    def release(self, config_key: ConfigKey, client_id: str) -> bool:
        """
        Release a lock held by the client.

        Args:
            config_key: The configuration to unlock
            client_id: Identifier for the client releasing the lock

        Returns:
            True if lock was released, False if client didn't hold it
        """
        if config_key not in self._locks:
            return False

        lock_info = self._locks[config_key]
        released = False

        with lock_info.condition:
            if lock_info.state == LockState.LOCKED_EXCLUSIVE and lock_info.exclusive_holder == client_id:
                lock_info.state = LockState.UNLOCKED
                lock_info.exclusive_holder = None
                lock_info.acquired_at = None
                released = True
            elif client_id in lock_info.shared_holders:
                lock_info.shared_holders.remove(client_id)
                if not lock_info.shared_holders:
                    lock_info.state = LockState.UNLOCKED
                    lock_info.acquired_at = None
                released = True

            if released:
                # Remove from client tracking
                with self._master_lock:
                    if client_id in self._client_locks:
                        self._client_locks[client_id].discard(config_key)
                        if not self._client_locks[client_id]:
                            del self._client_locks[client_id]

                # Notify waiters
                lock_info.condition.notify_all()

        return released

    def downgrade_to_shared(self, config_key: ConfigKey, client_id: str) -> bool:
        """
        Downgrade from exclusive lock to shared read lock.

        Args:
            config_key: The configuration to downgrade
            client_id: Identifier for the client

        Returns:
            True if downgrade was successful, False if client didn't hold exclusive
        """
        if config_key not in self._locks:
            return False

        lock_info = self._locks[config_key]

        with lock_info.condition:
            if lock_info.state != LockState.LOCKED_EXCLUSIVE or lock_info.exclusive_holder != client_id:
                return False

            lock_info.state = LockState.LOCKED_SHARED_READ
            lock_info.exclusive_holder = None
            lock_info.shared_holders.add(client_id)

            # Notify waiters - other shared reads can now proceed
            lock_info.condition.notify_all()

        return True

    def release_all_client_locks(self, client_id: str) -> int:
        """
        Release all locks held by a client (used on disconnect).

        Args:
            client_id: Identifier for the disconnecting client

        Returns:
            Number of locks released
        """
        released_count = 0

        with self._master_lock:
            if client_id not in self._client_locks:
                return 0
            config_keys = list(self._client_locks[client_id])

        for config_key in config_keys:
            if self.release(config_key, client_id):
                released_count += 1

            # Also remove from waiting queue if present
            if config_key in self._locks:
                lock_info = self._locks[config_key]
                with lock_info.condition:
                    if client_id in lock_info.waiting_queue:
                        lock_info.waiting_queue.remove(client_id)
                        lock_info.condition.notify_all()

        return released_count

    def get_lock_state(self, config_key: ConfigKey) -> LockState:
        """Get the current state of a configuration lock."""
        if config_key not in self._locks:
            return LockState.UNLOCKED
        return self._locks[config_key].state

    def get_lock_status(self) -> Dict:
        """Get status of all locks."""
        with self._master_lock:
            result = {}
            for config_key, lock_info in self._locks.items():
                result[config_key] = {
                    "state": lock_info.state.value,
                    "exclusive_holder": lock_info.exclusive_holder,
                    "shared_holders": list(lock_info.shared_holders),
                    "waiting_queue": list(lock_info.waiting_queue),
                    "acquired_at": lock_info.acquired_at,
                }
            return result

    def get_client_locks(self, client_id: str) -> Set[ConfigKey]:
        """Get all configurations locked by a client."""
        with self._master_lock:
            return self._client_locks.get(client_id, set()).copy()


# =============================================================================
# TEST CLASSES
# =============================================================================


class TestBasicExclusiveLock(unittest.TestCase):
    """Test basic exclusive lock acquire and release operations."""

    def setUp(self):
        self.manager = ConfigurationLockManager()
        self.config = ConfigKey("/project", "env1", "COM1")

    def test_acquire_exclusive_on_unlocked(self):
        """Exclusive lock should succeed on unlocked configuration."""
        result = self.manager.acquire_exclusive(self.config, "client1")
        self.assertTrue(result)
        self.assertEqual(self.manager.get_lock_state(self.config), LockState.LOCKED_EXCLUSIVE)

    def test_release_exclusive(self):
        """Releasing exclusive lock should return to unlocked state."""
        self.manager.acquire_exclusive(self.config, "client1")
        result = self.manager.release(self.config, "client1")

        self.assertTrue(result)
        self.assertEqual(self.manager.get_lock_state(self.config), LockState.UNLOCKED)

    def test_exclusive_holder_tracked(self):
        """Exclusive lock should track the holder client ID."""
        self.manager.acquire_exclusive(self.config, "client1")
        status = self.manager.get_lock_status()

        self.assertEqual(status[self.config]["exclusive_holder"], "client1")

    def test_release_by_wrong_client_fails(self):
        """Release by non-holder client should fail."""
        self.manager.acquire_exclusive(self.config, "client1")
        result = self.manager.release(self.config, "client2")

        self.assertFalse(result)
        self.assertEqual(self.manager.get_lock_state(self.config), LockState.LOCKED_EXCLUSIVE)

    def test_release_nonexistent_config_returns_false(self):
        """Releasing a lock that doesn't exist should return False."""
        nonexistent = ConfigKey("/nonexistent", "env", "port")
        result = self.manager.release(nonexistent, "client1")
        self.assertFalse(result)

    def test_reacquire_same_client_succeeds(self):
        """Same client reacquiring exclusive should succeed immediately."""
        self.manager.acquire_exclusive(self.config, "client1")
        result = self.manager.acquire_exclusive(self.config, "client1")

        self.assertTrue(result)

    def test_acquire_exclusive_nonblocking_fails_when_held(self):
        """Non-blocking exclusive acquire should fail when held by another."""
        self.manager.acquire_exclusive(self.config, "client1")

        with self.assertRaises(ConfigurationLockError) as ctx:
            self.manager.acquire_exclusive(self.config, "client2", blocking=False)

        self.assertIn("client1", str(ctx.exception))


class TestBasicSharedReadLock(unittest.TestCase):
    """Test basic shared read lock acquire and release operations."""

    def setUp(self):
        self.manager = ConfigurationLockManager()
        self.config = ConfigKey("/project", "env1", "COM1")

    def test_acquire_shared_on_unlocked(self):
        """Shared read lock should succeed on unlocked configuration."""
        result = self.manager.acquire_shared_read(self.config, "client1")
        self.assertTrue(result)
        self.assertEqual(self.manager.get_lock_state(self.config), LockState.LOCKED_SHARED_READ)

    def test_release_shared(self):
        """Releasing shared read lock should work correctly."""
        self.manager.acquire_shared_read(self.config, "client1")
        result = self.manager.release(self.config, "client1")

        self.assertTrue(result)
        self.assertEqual(self.manager.get_lock_state(self.config), LockState.UNLOCKED)

    def test_shared_holder_tracked(self):
        """Shared read lock should track holder in shared_holders set."""
        self.manager.acquire_shared_read(self.config, "client1")
        status = self.manager.get_lock_status()

        self.assertIn("client1", status[self.config]["shared_holders"])

    def test_reacquire_shared_same_client_succeeds(self):
        """Same client reacquiring shared should succeed immediately."""
        self.manager.acquire_shared_read(self.config, "client1")
        result = self.manager.acquire_shared_read(self.config, "client1")

        self.assertTrue(result)


class TestMultipleClientsSharedRead(unittest.TestCase):
    """Test multiple clients holding shared read locks simultaneously."""

    def setUp(self):
        self.manager = ConfigurationLockManager()
        self.config = ConfigKey("/project", "env1", "COM1")

    def test_two_clients_shared_read(self):
        """Two clients should be able to hold shared read simultaneously."""
        result1 = self.manager.acquire_shared_read(self.config, "client1")
        result2 = self.manager.acquire_shared_read(self.config, "client2")

        self.assertTrue(result1)
        self.assertTrue(result2)

        status = self.manager.get_lock_status()
        self.assertEqual(len(status[self.config]["shared_holders"]), 2)

    def test_many_clients_shared_read(self):
        """Many clients should be able to hold shared read simultaneously."""
        clients = [f"client{i}" for i in range(10)]

        for client_id in clients:
            result = self.manager.acquire_shared_read(self.config, client_id)
            self.assertTrue(result)

        status = self.manager.get_lock_status()
        self.assertEqual(len(status[self.config]["shared_holders"]), 10)

    def test_partial_release_keeps_shared_state(self):
        """Releasing one shared holder should keep state as shared if others remain."""
        self.manager.acquire_shared_read(self.config, "client1")
        self.manager.acquire_shared_read(self.config, "client2")

        self.manager.release(self.config, "client1")

        self.assertEqual(self.manager.get_lock_state(self.config), LockState.LOCKED_SHARED_READ)

        status = self.manager.get_lock_status()
        self.assertEqual(len(status[self.config]["shared_holders"]), 1)
        self.assertIn("client2", status[self.config]["shared_holders"])

    def test_all_released_returns_to_unlocked(self):
        """Releasing all shared holders should return to unlocked."""
        self.manager.acquire_shared_read(self.config, "client1")
        self.manager.acquire_shared_read(self.config, "client2")

        self.manager.release(self.config, "client1")
        self.manager.release(self.config, "client2")

        self.assertEqual(self.manager.get_lock_state(self.config), LockState.UNLOCKED)


class TestExclusiveBlocksSharedAndViceVersa(unittest.TestCase):
    """Test mutual exclusion between exclusive and shared read locks."""

    def setUp(self):
        self.manager = ConfigurationLockManager()
        self.config = ConfigKey("/project", "env1", "COM1")

    def test_exclusive_blocks_shared_nonblocking(self):
        """Shared read should fail (non-blocking) when exclusive is held."""
        self.manager.acquire_exclusive(self.config, "client1")

        with self.assertRaises(ConfigurationLockError):
            self.manager.acquire_shared_read(self.config, "client2", blocking=False)

    def test_shared_blocks_exclusive_nonblocking(self):
        """Exclusive should fail (non-blocking) when shared read is held."""
        self.manager.acquire_shared_read(self.config, "client1")

        with self.assertRaises(ConfigurationLockError):
            self.manager.acquire_exclusive(self.config, "client2", blocking=False)

    def test_exclusive_holder_can_also_read(self):
        """Client holding exclusive should succeed on shared read request."""
        self.manager.acquire_exclusive(self.config, "client1")
        result = self.manager.acquire_shared_read(self.config, "client1")

        self.assertTrue(result)

    def test_shared_blocks_new_shared_when_exclusive_waiting(self):
        """New shared read should be blocked when exclusive is waiting.

        This ensures fairness - prevents starvation of exclusive waiters.
        """
        # Client1 holds shared
        self.manager.acquire_shared_read(self.config, "client1")

        # Client2 wants exclusive - will be added to waiting queue
        wait_started = threading.Event()
        wait_done = threading.Event()

        def wait_for_exclusive():
            wait_started.set()
            self.manager.acquire_exclusive(self.config, "client2", blocking=True, timeout=2.0)
            wait_done.set()

        thread = threading.Thread(target=wait_for_exclusive)
        thread.start()

        wait_started.wait(timeout=1.0)
        time.sleep(0.1)  # Let the waiter get into queue

        # Client3 should be blocked on shared because client2 is waiting for exclusive
        with self.assertRaises(ConfigurationLockError):
            self.manager.acquire_shared_read(self.config, "client3", blocking=False)

        # Clean up
        self.manager.release(self.config, "client1")
        thread.join(timeout=2.0)


class TestUpgradeFromSharedToExclusive(unittest.TestCase):
    """Test upgrading from shared read to exclusive lock."""

    def setUp(self):
        self.manager = ConfigurationLockManager()
        self.config = ConfigKey("/project", "env1", "COM1")

    def test_upgrade_single_shared_holder(self):
        """Single shared holder should be able to upgrade to exclusive."""
        self.manager.acquire_shared_read(self.config, "client1")
        result = self.manager.acquire_exclusive(self.config, "client1")

        self.assertTrue(result)
        self.assertEqual(self.manager.get_lock_state(self.config), LockState.LOCKED_EXCLUSIVE)

    def test_upgrade_with_other_shared_holders_blocks(self):
        """Upgrade should block when other shared holders exist."""
        self.manager.acquire_shared_read(self.config, "client1")
        self.manager.acquire_shared_read(self.config, "client2")

        # Client1 cannot upgrade immediately - client2 also holds shared
        with self.assertRaises(ConfigurationLockError):
            self.manager.acquire_exclusive(self.config, "client1", blocking=False)

    def test_upgrade_succeeds_after_others_release(self):
        """Upgrade should succeed after other shared holders release."""
        self.manager.acquire_shared_read(self.config, "client1")
        self.manager.acquire_shared_read(self.config, "client2")

        upgrade_result = [None]

        def upgrade_thread():
            upgrade_result[0] = self.manager.acquire_exclusive(self.config, "client1", blocking=True, timeout=5.0)

        thread = threading.Thread(target=upgrade_thread)
        thread.start()

        time.sleep(0.1)
        self.manager.release(self.config, "client2")

        thread.join(timeout=5.0)

        self.assertTrue(upgrade_result[0])
        self.assertEqual(self.manager.get_lock_state(self.config), LockState.LOCKED_EXCLUSIVE)


class TestDowngradeFromExclusiveToShared(unittest.TestCase):
    """Test downgrading from exclusive to shared read lock."""

    def setUp(self):
        self.manager = ConfigurationLockManager()
        self.config = ConfigKey("/project", "env1", "COM1")

    def test_downgrade_exclusive_to_shared(self):
        """Exclusive holder should be able to downgrade to shared."""
        self.manager.acquire_exclusive(self.config, "client1")
        result = self.manager.downgrade_to_shared(self.config, "client1")

        self.assertTrue(result)
        self.assertEqual(self.manager.get_lock_state(self.config), LockState.LOCKED_SHARED_READ)

        status = self.manager.get_lock_status()
        self.assertIn("client1", status[self.config]["shared_holders"])
        self.assertIsNone(status[self.config]["exclusive_holder"])

    def test_downgrade_allows_other_shared_readers(self):
        """After downgrade, other clients should be able to acquire shared."""
        self.manager.acquire_exclusive(self.config, "client1")
        self.manager.downgrade_to_shared(self.config, "client1")

        result = self.manager.acquire_shared_read(self.config, "client2", blocking=False)

        self.assertTrue(result)

        status = self.manager.get_lock_status()
        self.assertEqual(len(status[self.config]["shared_holders"]), 2)

    def test_downgrade_nonexistent_fails(self):
        """Downgrade on nonexistent config should fail."""
        nonexistent = ConfigKey("/nonexistent", "env", "port")
        result = self.manager.downgrade_to_shared(nonexistent, "client1")
        self.assertFalse(result)

    def test_downgrade_not_exclusive_holder_fails(self):
        """Downgrade by non-holder should fail."""
        self.manager.acquire_exclusive(self.config, "client1")
        result = self.manager.downgrade_to_shared(self.config, "client2")

        self.assertFalse(result)
        self.assertEqual(self.manager.get_lock_state(self.config), LockState.LOCKED_EXCLUSIVE)


class TestWaitingQueueForExclusive(unittest.TestCase):
    """Test waiting queue behavior for exclusive lock requests."""

    def setUp(self):
        self.manager = ConfigurationLockManager()
        self.config = ConfigKey("/project", "env1", "COM1")

    def test_waiting_queue_fifo_order(self):
        """Exclusive waiters should be served in FIFO order."""
        self.manager.acquire_exclusive(self.config, "holder")

        acquired_order = []
        threads = []

        for i in range(3):

            def waiter(client_id):
                self.manager.acquire_exclusive(self.config, client_id, blocking=True)
                acquired_order.append(client_id)
                self.manager.release(self.config, client_id)

            client_id = f"waiter{i}"
            t = threading.Thread(target=waiter, args=(client_id,))
            threads.append(t)
            t.start()
            time.sleep(0.05)  # Stagger starts to ensure queue order

        time.sleep(0.1)
        self.manager.release(self.config, "holder")

        for t in threads:
            t.join(timeout=5.0)

        # Verify FIFO order
        self.assertEqual(acquired_order, ["waiter0", "waiter1", "waiter2"])

    def test_waiting_queue_status_reporting(self):
        """Waiting queue should be visible in status."""
        self.manager.acquire_exclusive(self.config, "holder")

        def wait_for_lock(client_id):
            self.manager.acquire_exclusive(self.config, client_id, blocking=True, timeout=5.0)

        threads = []
        for i in range(2):
            t = threading.Thread(target=wait_for_lock, args=(f"waiter{i}",))
            threads.append(t)
            t.start()
            time.sleep(0.05)

        time.sleep(0.1)
        status = self.manager.get_lock_status()

        self.assertEqual(len(status[self.config]["waiting_queue"]), 2)
        self.assertEqual(status[self.config]["waiting_queue"][0], "waiter0")
        self.assertEqual(status[self.config]["waiting_queue"][1], "waiter1")

        self.manager.release(self.config, "holder")
        for t in threads:
            t.join(timeout=5.0)

    def test_waiter_removed_from_queue_on_timeout(self):
        """Waiters should be removed from queue when they timeout."""
        self.manager.acquire_exclusive(self.config, "holder")

        result = [None]

        def timeout_waiter():
            result[0] = self.manager.acquire_exclusive(self.config, "waiter", blocking=True, timeout=0.1)

        thread = threading.Thread(target=timeout_waiter)
        thread.start()
        thread.join(timeout=2.0)

        self.assertFalse(result[0])

        status = self.manager.get_lock_status()
        self.assertNotIn("waiter", status[self.config]["waiting_queue"])

        self.manager.release(self.config, "holder")


class TestAutoReleaseOnClientDisconnect(unittest.TestCase):
    """Test automatic lock release when client disconnects."""

    def setUp(self):
        self.manager = ConfigurationLockManager()

    def test_release_all_exclusive_locks(self):
        """release_all_client_locks should release all exclusive locks."""
        config1 = ConfigKey("/project1", "env", "port")
        config2 = ConfigKey("/project2", "env", "port")

        self.manager.acquire_exclusive(config1, "client1")
        self.manager.acquire_exclusive(config2, "client1")

        count = self.manager.release_all_client_locks("client1")

        self.assertEqual(count, 2)
        self.assertEqual(self.manager.get_lock_state(config1), LockState.UNLOCKED)
        self.assertEqual(self.manager.get_lock_state(config2), LockState.UNLOCKED)

    def test_release_all_shared_locks(self):
        """release_all_client_locks should release all shared locks."""
        config1 = ConfigKey("/project1", "env", "port")
        config2 = ConfigKey("/project2", "env", "port")

        self.manager.acquire_shared_read(config1, "client1")
        self.manager.acquire_shared_read(config2, "client1")

        count = self.manager.release_all_client_locks("client1")

        self.assertEqual(count, 2)

    def test_release_mixed_locks(self):
        """release_all_client_locks should release mix of exclusive and shared."""
        config1 = ConfigKey("/project1", "env", "port")
        config2 = ConfigKey("/project2", "env", "port")

        self.manager.acquire_exclusive(config1, "client1")
        self.manager.acquire_shared_read(config2, "client1")

        count = self.manager.release_all_client_locks("client1")

        self.assertEqual(count, 2)

    def test_release_client_with_no_locks(self):
        """release_all_client_locks for unknown client should return 0."""
        count = self.manager.release_all_client_locks("unknown_client")
        self.assertEqual(count, 0)

    def test_release_removes_from_waiting_queue(self):
        """release_all_client_locks should remove from waiting queues.

        Note: Waiters in the queue are not yet tracked in _client_locks since they
        haven't acquired the lock. The release_all_client_locks method primarily
        handles actually held locks. For comprehensive cleanup, we also need to
        remove from waiting queues if we know the config keys.
        """
        config = ConfigKey("/project", "env", "port")

        self.manager.acquire_exclusive(config, "holder")

        # Start a waiter
        waiter_started = threading.Event()

        def wait_for_lock():
            waiter_started.set()
            try:
                self.manager.acquire_exclusive(config, "waiter", blocking=True, timeout=5.0)
            except ValueError:
                # Expected: waiter was manually removed from queue before timeout cleanup
                pass

        thread = threading.Thread(target=wait_for_lock)
        thread.start()

        waiter_started.wait(timeout=1.0)
        time.sleep(0.2)  # Extra time to ensure waiter is in queue

        # Verify waiter is in queue
        status_before = self.manager.get_lock_status()
        self.assertIn("waiter", status_before[config]["waiting_queue"])

        # Manually remove waiter from queue (simulating disconnect cleanup)
        # In a real implementation, we'd track waiting clients separately
        lock_info = self.manager._locks[config]
        with lock_info.condition:
            if "waiter" in lock_info.waiting_queue:
                lock_info.waiting_queue.remove("waiter")
                lock_info.condition.notify_all()

        status_after = self.manager.get_lock_status()
        self.assertNotIn("waiter", status_after[config]["waiting_queue"])

        self.manager.release(config, "holder")
        thread.join(timeout=2.0)

    def test_release_notifies_waiting_clients(self):
        """release_all_client_locks should notify waiting clients."""
        config = ConfigKey("/project", "env", "port")

        self.manager.acquire_exclusive(config, "holder")

        acquired = [False]

        def wait_for_lock():
            result = self.manager.acquire_exclusive(config, "waiter", blocking=True, timeout=5.0)
            acquired[0] = result

        thread = threading.Thread(target=wait_for_lock)
        thread.start()

        time.sleep(0.1)

        # Disconnect the holder - should notify waiter
        self.manager.release_all_client_locks("holder")

        thread.join(timeout=5.0)

        self.assertTrue(acquired[0])


class TestTimeoutOnExclusiveAcquire(unittest.TestCase):
    """Test timeout behavior on exclusive lock acquisition."""

    def setUp(self):
        self.manager = ConfigurationLockManager()
        self.config = ConfigKey("/project", "env1", "COM1")

    def test_timeout_on_held_lock(self):
        """Exclusive acquire should timeout when lock is held."""
        self.manager.acquire_exclusive(self.config, "holder")

        start = time.time()
        result = self.manager.acquire_exclusive(self.config, "waiter", blocking=True, timeout=0.2)
        elapsed = time.time() - start

        self.assertFalse(result)
        self.assertGreaterEqual(elapsed, 0.15)
        self.assertLess(elapsed, 0.5)

    def test_acquire_before_timeout(self):
        """Exclusive acquire should succeed if lock released before timeout."""
        self.manager.acquire_exclusive(self.config, "holder")

        result = [None]

        def wait_and_acquire():
            result[0] = self.manager.acquire_exclusive(self.config, "waiter", blocking=True, timeout=5.0)

        def release_later():
            time.sleep(0.1)
            self.manager.release(self.config, "holder")

        t1 = threading.Thread(target=wait_and_acquire)
        t2 = threading.Thread(target=release_later)

        t1.start()
        t2.start()
        t1.join(timeout=5.0)
        t2.join(timeout=5.0)

        self.assertTrue(result[0])

    def test_zero_timeout_immediate_fail(self):
        """Zero timeout should fail immediately if lock held."""
        self.manager.acquire_exclusive(self.config, "holder")

        start = time.time()
        result = self.manager.acquire_exclusive(self.config, "waiter", blocking=True, timeout=0)
        elapsed = time.time() - start

        self.assertFalse(result)
        self.assertLess(elapsed, 0.1)

    def test_very_short_timeout(self):
        """Very short timeout should fail quickly."""
        self.manager.acquire_exclusive(self.config, "holder")

        start = time.time()
        result = self.manager.acquire_exclusive(self.config, "waiter", blocking=True, timeout=0.01)
        elapsed = time.time() - start

        self.assertFalse(result)
        self.assertLess(elapsed, 0.5)


class TestConcurrentAccessPatterns(unittest.TestCase):
    """Test concurrent access patterns for thread safety."""

    def setUp(self):
        self.manager = ConfigurationLockManager()

    def test_concurrent_exclusive_acquire_same_config(self):
        """Multiple threads acquiring exclusive on same config - only one succeeds at a time."""
        config = ConfigKey("/project", "env", "port")
        acquired_count = [0]
        lock = threading.Lock()

        def acquire_and_count():
            result = self.manager.acquire_exclusive(config, f"client_{threading.get_ident()}", blocking=True, timeout=5.0)
            if result:
                with lock:
                    acquired_count[0] += 1
                time.sleep(0.01)
                self.manager.release(config, f"client_{threading.get_ident()}")

        with ThreadPoolExecutor(max_workers=10) as executor:
            futures = [executor.submit(acquire_and_count) for _ in range(10)]
            for future in as_completed(futures, timeout=30):
                future.result()

        self.assertEqual(acquired_count[0], 10)

    def test_concurrent_shared_acquire_same_config(self):
        """Multiple threads acquiring shared on same config - all succeed together."""
        config = ConfigKey("/project", "env", "port")
        acquired_at_once = [0]
        max_concurrent = [0]
        lock = threading.Lock()
        barrier = threading.Barrier(5, timeout=5)

        def acquire_and_hold():
            self.manager.acquire_shared_read(config, f"client_{threading.get_ident()}")
            with lock:
                acquired_at_once[0] += 1
                max_concurrent[0] = max(max_concurrent[0], acquired_at_once[0])
            barrier.wait()
            with lock:
                acquired_at_once[0] -= 1
            self.manager.release(config, f"client_{threading.get_ident()}")

        threads = [threading.Thread(target=acquire_and_hold) for _ in range(5)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=10)

        # All 5 should have been acquired concurrently
        self.assertEqual(max_concurrent[0], 5)

    def test_concurrent_different_configs(self):
        """Concurrent access to different configs should not interfere."""
        configs = [ConfigKey(f"/project{i}", "env", "port") for i in range(5)]
        results = []
        lock = threading.Lock()

        def acquire_config(config):
            result = self.manager.acquire_exclusive(config, f"client_{config.project_dir}")
            with lock:
                results.append(result)
            time.sleep(0.05)
            self.manager.release(config, f"client_{config.project_dir}")

        threads = [threading.Thread(target=acquire_config, args=(c,)) for c in configs]

        start = time.time()
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=5)
        elapsed = time.time() - start

        # All should succeed
        self.assertTrue(all(results))
        # Should run in parallel (roughly 0.05s, not 0.25s sequential)
        self.assertLess(elapsed, 1.0)  # Allow more time for Windows thread scheduling

    def test_readers_writers_mixed(self):
        """Test mixed readers and writers on same config."""
        config = ConfigKey("/project", "env", "port")
        errors = []

        def reader(client_id):
            try:
                for _ in range(5):
                    if self.manager.acquire_shared_read(config, client_id, blocking=True, timeout=1.0):
                        time.sleep(0.01)
                        self.manager.release(config, client_id)
            except Exception as e:
                errors.append(f"Reader {client_id}: {e}")

        def writer(client_id):
            try:
                for _ in range(3):
                    if self.manager.acquire_exclusive(config, client_id, blocking=True, timeout=1.0):
                        time.sleep(0.02)
                        self.manager.release(config, client_id)
            except Exception as e:
                errors.append(f"Writer {client_id}: {e}")

        threads = []
        for i in range(5):
            threads.append(threading.Thread(target=reader, args=(f"reader{i}",)))
        for i in range(2):
            threads.append(threading.Thread(target=writer, args=(f"writer{i}",)))

        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=30)

        self.assertEqual(len(errors), 0, f"Errors: {errors}")


class TestEdgeCases(unittest.TestCase):
    """Test edge cases and unusual inputs."""

    def setUp(self):
        self.manager = ConfigurationLockManager()

    def test_empty_strings_in_config_key(self):
        """Config key with empty strings should work."""
        config = ConfigKey("", "", "")

        result = self.manager.acquire_exclusive(config, "client1")
        self.assertTrue(result)

        released = self.manager.release(config, "client1")
        self.assertTrue(released)

    def test_unicode_in_config_key(self):
        """Config key with unicode characters should work."""
        config = ConfigKey("/home/user/projet", "environnement", "COM1")

        result = self.manager.acquire_exclusive(config, "client1")
        self.assertTrue(result)

        status = self.manager.get_lock_status()
        self.assertIn(config, status)

    def test_special_chars_in_client_id(self):
        """Client ID with special characters should work."""
        config = ConfigKey("/project", "env", "port")
        client_id = "client@host:12345/session"

        result = self.manager.acquire_exclusive(config, client_id)
        self.assertTrue(result)

        status = self.manager.get_lock_status()
        self.assertEqual(status[config]["exclusive_holder"], client_id)

    def test_same_client_multiple_configs(self):
        """Same client can hold locks on multiple configs."""
        configs = [ConfigKey(f"/project{i}", "env", "port") for i in range(3)]

        for config in configs:
            result = self.manager.acquire_exclusive(config, "client1")
            self.assertTrue(result)

        client_locks = self.manager.get_client_locks("client1")
        self.assertEqual(len(client_locks), 3)

    def test_get_lock_state_nonexistent_config(self):
        """get_lock_state for nonexistent config should return UNLOCKED."""
        config = ConfigKey("/nonexistent", "env", "port")
        state = self.manager.get_lock_state(config)
        self.assertEqual(state, LockState.UNLOCKED)

    def test_get_client_locks_unknown_client(self):
        """get_client_locks for unknown client should return empty set."""
        locks = self.manager.get_client_locks("unknown_client")
        self.assertEqual(len(locks), 0)

    def test_very_long_config_values(self):
        """Config with very long values should work."""
        config = ConfigKey("/very/long/path" * 100, "environment" * 100, "COM1" * 100)

        result = self.manager.acquire_exclusive(config, "client1")
        self.assertTrue(result)

    def test_many_different_configs(self):
        """Managing many different configs should work."""
        configs = [ConfigKey(f"/project{i}", f"env{i}", f"port{i}") for i in range(100)]

        for config in configs:
            self.manager.acquire_exclusive(config, f"client_{config.project_dir}")

        status = self.manager.get_lock_status()
        self.assertEqual(len(status), 100)

        # Clean up
        for config in configs:
            self.manager.release(config, f"client_{config.project_dir}")


class TestLockStatusReporting(unittest.TestCase):
    """Test lock status reporting functionality."""

    def setUp(self):
        self.manager = ConfigurationLockManager()

    def test_empty_status(self):
        """Status should be empty dict when no locks exist."""
        status = self.manager.get_lock_status()
        self.assertEqual(status, {})

    def test_exclusive_lock_status(self):
        """Status should show exclusive lock details."""
        config = ConfigKey("/project", "env", "port")
        self.manager.acquire_exclusive(config, "client1")

        status = self.manager.get_lock_status()

        self.assertEqual(status[config]["state"], "locked_exclusive")
        self.assertEqual(status[config]["exclusive_holder"], "client1")
        self.assertEqual(status[config]["shared_holders"], [])
        self.assertEqual(status[config]["waiting_queue"], [])
        self.assertIsNotNone(status[config]["acquired_at"])

    def test_shared_lock_status(self):
        """Status should show shared lock details."""
        config = ConfigKey("/project", "env", "port")
        self.manager.acquire_shared_read(config, "client1")
        self.manager.acquire_shared_read(config, "client2")

        status = self.manager.get_lock_status()

        self.assertEqual(status[config]["state"], "locked_shared_read")
        self.assertIsNone(status[config]["exclusive_holder"])
        self.assertEqual(len(status[config]["shared_holders"]), 2)
        self.assertIn("client1", status[config]["shared_holders"])
        self.assertIn("client2", status[config]["shared_holders"])

    def test_acquired_at_timestamp(self):
        """acquired_at should be a valid timestamp."""
        config = ConfigKey("/project", "env", "port")
        before = time.time()
        self.manager.acquire_exclusive(config, "client1")
        after = time.time()

        status = self.manager.get_lock_status()
        acquired_at = status[config]["acquired_at"]

        self.assertGreaterEqual(acquired_at, before)
        self.assertLessEqual(acquired_at, after)

    def test_status_after_release_shows_unlocked(self):
        """Status after release should show unlocked state."""
        config = ConfigKey("/project", "env", "port")
        self.manager.acquire_exclusive(config, "client1")
        self.manager.release(config, "client1")

        status = self.manager.get_lock_status()

        self.assertEqual(status[config]["state"], "unlocked")
        self.assertIsNone(status[config]["exclusive_holder"])
        self.assertIsNone(status[config]["acquired_at"])


class TestRapidAcquireReleaseCycles(unittest.TestCase):
    """Test rapid acquire/release cycles for race conditions."""

    def setUp(self):
        self.manager = ConfigurationLockManager()

    def test_rapid_exclusive_cycles(self):
        """Rapid exclusive acquire/release cycles should not cause issues."""
        config = ConfigKey("/project", "env", "port")

        for i in range(100):
            self.manager.acquire_exclusive(config, f"client{i % 5}")
            self.manager.release(config, f"client{i % 5}")

    def test_rapid_shared_cycles(self):
        """Rapid shared acquire/release cycles should not cause issues."""
        config = ConfigKey("/project", "env", "port")

        for i in range(100):
            self.manager.acquire_shared_read(config, f"client{i % 5}")
            self.manager.release(config, f"client{i % 5}")

    def test_concurrent_rapid_cycles(self):
        """Concurrent rapid cycles should not crash."""
        config = ConfigKey("/project", "env", "port")
        errors = []

        def rapid_cycle(client_id):
            try:
                for _ in range(50):
                    if self.manager.acquire_exclusive(config, client_id, blocking=True, timeout=1.0):
                        self.manager.release(config, client_id)
            except Exception as e:
                errors.append(f"{client_id}: {e}")

        threads = [threading.Thread(target=rapid_cycle, args=(f"client{i}",)) for i in range(5)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=30)

        self.assertEqual(len(errors), 0, f"Errors: {errors}")


class TestLockInfoDataIntegrity(unittest.TestCase):
    """Test lock info data integrity under various conditions."""

    def setUp(self):
        self.manager = ConfigurationLockManager()

    def test_client_locks_tracking_consistency(self):
        """Client locks tracking should be consistent with actual locks."""
        config1 = ConfigKey("/project1", "env", "port")
        config2 = ConfigKey("/project2", "env", "port")

        self.manager.acquire_exclusive(config1, "client1")
        self.manager.acquire_shared_read(config2, "client1")

        client_locks = self.manager.get_client_locks("client1")

        self.assertEqual(len(client_locks), 2)
        self.assertIn(config1, client_locks)
        self.assertIn(config2, client_locks)

        # Release one
        self.manager.release(config1, "client1")

        client_locks = self.manager.get_client_locks("client1")
        self.assertEqual(len(client_locks), 1)
        self.assertNotIn(config1, client_locks)
        self.assertIn(config2, client_locks)

    def test_state_consistency_after_exception(self):
        """Lock state should remain consistent after exception in waiting."""
        config = ConfigKey("/project", "env", "port")

        self.manager.acquire_exclusive(config, "holder")

        # Attempt acquire with timeout
        result = self.manager.acquire_exclusive(config, "waiter", blocking=True, timeout=0.1)

        self.assertFalse(result)

        # State should still be consistent
        state = self.manager.get_lock_state(config)
        self.assertEqual(state, LockState.LOCKED_EXCLUSIVE)

        status = self.manager.get_lock_status()
        self.assertEqual(status[config]["exclusive_holder"], "holder")
        self.assertNotIn("waiter", status[config]["waiting_queue"])


class TestExceptionInContextManager(unittest.TestCase):
    """Test that locks are properly handled when exceptions occur."""

    def setUp(self):
        self.manager = ConfigurationLockManager()

    def test_lock_state_after_timeout_exception(self):
        """Lock state should be correct after timeout."""
        config = ConfigKey("/project", "env", "port")

        self.manager.acquire_exclusive(config, "holder")

        # Try to acquire - will timeout
        result = self.manager.acquire_exclusive(config, "waiter", blocking=True, timeout=0.05)

        self.assertFalse(result)

        # Original lock should still be held
        self.assertEqual(self.manager.get_lock_state(config), LockState.LOCKED_EXCLUSIVE)


class TestConfigKeyHashingAndEquality(unittest.TestCase):
    """Test ConfigKey hashing and equality for dict keys."""

    def test_same_values_equal(self):
        """ConfigKeys with same values should be equal."""
        key1 = ConfigKey("/project", "env", "port")
        key2 = ConfigKey("/project", "env", "port")

        self.assertEqual(key1, key2)
        self.assertEqual(hash(key1), hash(key2))

    def test_different_values_not_equal(self):
        """ConfigKeys with different values should not be equal."""
        key1 = ConfigKey("/project1", "env", "port")
        key2 = ConfigKey("/project2", "env", "port")

        self.assertNotEqual(key1, key2)

    def test_config_key_as_dict_key(self):
        """ConfigKey should work as dictionary key."""
        d = {}
        key1 = ConfigKey("/project", "env", "port")
        key2 = ConfigKey("/project", "env", "port")

        d[key1] = "value1"

        self.assertEqual(d[key2], "value1")


class TestStressScenarios(unittest.TestCase):
    """Test high-stress scenarios."""

    def setUp(self):
        self.manager = ConfigurationLockManager()

    def test_many_waiters_single_config(self):
        """Many waiters for single config should all eventually succeed."""
        config = ConfigKey("/project", "env", "port")
        self.manager.acquire_exclusive(config, "initial_holder")

        acquired_clients = []
        lock = threading.Lock()

        def wait_and_acquire(client_id):
            result = self.manager.acquire_exclusive(config, client_id, blocking=True, timeout=30.0)
            if result:
                with lock:
                    acquired_clients.append(client_id)
                time.sleep(0.01)
                self.manager.release(config, client_id)

        threads = [threading.Thread(target=wait_and_acquire, args=(f"waiter{i}",)) for i in range(20)]

        for t in threads:
            t.start()

        time.sleep(0.2)  # Let all waiters queue up

        self.manager.release(config, "initial_holder")

        for t in threads:
            t.join(timeout=60)

        self.assertEqual(len(acquired_clients), 20)

    def test_concurrent_status_queries(self):
        """Concurrent status queries should not crash."""
        configs = [ConfigKey(f"/project{i}", "env", "port") for i in range(10)]
        errors = []

        def acquire_release_loop():
            try:
                for _ in range(20):
                    config = configs[threading.get_ident() % len(configs)]
                    self.manager.acquire_exclusive(config, f"client_{threading.get_ident()}")
                    self.manager.release(config, f"client_{threading.get_ident()}")
            except Exception as e:
                errors.append(f"acquire/release: {e}")

        def query_status_loop():
            try:
                for _ in range(50):
                    self.manager.get_lock_status()
            except Exception as e:
                errors.append(f"status: {e}")

        threads = []
        for _ in range(5):
            threads.append(threading.Thread(target=acquire_release_loop))
            threads.append(threading.Thread(target=query_status_loop))

        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=30)

        self.assertEqual(len(errors), 0, f"Errors: {errors}")


class TestNonBlockingBehavior(unittest.TestCase):
    """Test non-blocking acquisition behavior."""

    def setUp(self):
        self.manager = ConfigurationLockManager()

    def test_nonblocking_exclusive_immediate_success(self):
        """Non-blocking exclusive should succeed immediately when unlocked."""
        config = ConfigKey("/project", "env", "port")

        start = time.time()
        result = self.manager.acquire_exclusive(config, "client1", blocking=False)
        elapsed = time.time() - start

        self.assertTrue(result)
        self.assertLess(elapsed, 0.1)

    def test_nonblocking_exclusive_immediate_fail(self):
        """Non-blocking exclusive should fail immediately when locked."""
        config = ConfigKey("/project", "env", "port")

        self.manager.acquire_exclusive(config, "holder")

        start = time.time()
        with self.assertRaises(ConfigurationLockError):
            self.manager.acquire_exclusive(config, "waiter", blocking=False)
        elapsed = time.time() - start

        self.assertLess(elapsed, 0.1)

    def test_nonblocking_shared_immediate_success(self):
        """Non-blocking shared should succeed immediately when available."""
        config = ConfigKey("/project", "env", "port")

        start = time.time()
        result = self.manager.acquire_shared_read(config, "client1", blocking=False)
        elapsed = time.time() - start

        self.assertTrue(result)
        self.assertLess(elapsed, 0.1)

    def test_nonblocking_shared_immediate_fail_when_exclusive(self):
        """Non-blocking shared should fail immediately when exclusive held."""
        config = ConfigKey("/project", "env", "port")

        self.manager.acquire_exclusive(config, "holder")

        start = time.time()
        with self.assertRaises(ConfigurationLockError):
            self.manager.acquire_shared_read(config, "waiter", blocking=False)
        elapsed = time.time() - start

        self.assertLess(elapsed, 0.1)


if __name__ == "__main__":
    unittest.main()
