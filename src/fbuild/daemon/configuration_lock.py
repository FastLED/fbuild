"""
Configuration Lock Manager - Lock management for daemon configuration resources.

This module provides the ConfigurationLockManager class which manages locks
for specific configurations identified by (project_dir, environment, port) tuples.
Key features:
- Configuration-based locking with composite keys
- Exclusive locks for build/deploy operations (single holder)
- Shared read locks for monitoring operations (multiple holders)
- Async client connection tracking with auto-release on disconnect
- Waiting queue for exclusive lock requests
- Lock upgrade/downgrade support (shared <-> exclusive)
"""

import logging
import threading
import time
from collections import deque
from dataclasses import dataclass, field
from enum import Enum
from typing import Any

# Default timeout for exclusive lock acquisition: 5 minutes
DEFAULT_EXCLUSIVE_TIMEOUT = 300.0

# Default lock expiry: 30 minutes (for long builds)
DEFAULT_LOCK_EXPIRY = 1800.0


class LockState(Enum):
    """Lock state enumeration."""

    UNLOCKED = "unlocked"
    LOCKED_EXCLUSIVE = "locked_exclusive"
    LOCKED_SHARED_READ = "locked_shared_read"


@dataclass
class WaitingRequest:
    """Represents a waiting request for an exclusive lock.

    Attributes:
        client_id: UUID string identifying the waiting client
        description: Human-readable description of the operation
        requested_at: Unix timestamp when request was made
        event: Threading event to signal when lock is granted
    """

    client_id: str
    description: str
    requested_at: float = field(default_factory=time.time)
    event: threading.Event = field(default_factory=threading.Event)


@dataclass
class LockHolder:
    """Information about a client holding a lock.

    Attributes:
        client_id: UUID string identifying the client
        description: Human-readable description of the operation
        acquired_at: Unix timestamp when lock was acquired
        lock_type: Type of lock held (exclusive or shared_read)
    """

    client_id: str
    description: str
    acquired_at: float = field(default_factory=time.time)
    lock_type: str = "exclusive"  # "exclusive" or "shared_read"

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return {
            "client_id": self.client_id,
            "description": self.description,
            "acquired_at": self.acquired_at,
            "lock_type": self.lock_type,
            "hold_duration": time.time() - self.acquired_at,
        }


@dataclass
class ConfigurationLock:
    """Lock state and metadata for a specific configuration.

    A configuration is identified by the tuple (project_dir, environment, port).

    Attributes:
        config_key: The configuration key tuple
        state: Current lock state
        exclusive_holder: Client holding exclusive lock (if any)
        shared_holders: Dict of client_id -> LockHolder for shared read locks
        waiting_queue: Queue of clients waiting for exclusive lock
        created_at: Unix timestamp when lock was created
        last_activity_at: Unix timestamp of last lock activity
    """

    config_key: tuple[str, str, str]
    state: LockState = LockState.UNLOCKED
    exclusive_holder: LockHolder | None = None
    shared_holders: dict[str, LockHolder] = field(default_factory=dict)
    waiting_queue: deque[WaitingRequest] = field(default_factory=deque)
    created_at: float = field(default_factory=time.time)
    last_activity_at: float = field(default_factory=time.time)

    def is_held(self) -> bool:
        """Check if lock is currently held by any client."""
        return self.state != LockState.UNLOCKED

    def holder_count(self) -> int:
        """Get number of clients holding the lock."""
        if self.state == LockState.LOCKED_EXCLUSIVE:
            return 1 if self.exclusive_holder else 0
        elif self.state == LockState.LOCKED_SHARED_READ:
            return len(self.shared_holders)
        return 0

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        waiting_list = [
            {
                "client_id": req.client_id,
                "description": req.description,
                "requested_at": req.requested_at,
                "wait_duration": time.time() - req.requested_at,
            }
            for req in self.waiting_queue
        ]

        return {
            "config_key": {
                "project_dir": self.config_key[0],
                "environment": self.config_key[1],
                "port": self.config_key[2],
            },
            "state": self.state.value,
            "exclusive_holder": self.exclusive_holder.to_dict() if self.exclusive_holder else None,
            "shared_holders": {client_id: holder.to_dict() for client_id, holder in self.shared_holders.items()},
            "waiting_queue": waiting_list,
            "waiting_count": len(self.waiting_queue),
            "holder_count": self.holder_count(),
            "created_at": self.created_at,
            "last_activity_at": self.last_activity_at,
        }


class ConfigurationLockError(RuntimeError):
    """Error raised when a configuration lock operation fails.

    Provides detailed information about the lock state and failure reason.
    """

    def __init__(
        self,
        message: str,
        config_key: tuple[str, str, str],
        lock_info: ConfigurationLock | None = None,
    ):
        self.config_key = config_key
        self.lock_info = lock_info
        super().__init__(message)


class ConfigurationLockManager:
    """Manages configuration locks for daemon operations.

    This class provides locking for specific configurations identified by
    (project_dir, environment, port) tuples. It supports:
    - Exclusive locks for build/deploy operations (only one holder)
    - Shared read locks for monitoring operations (multiple holders)
    - Async client tracking with auto-release on disconnect
    - Waiting queue for exclusive lock requests
    - Lock upgrade/downgrade between shared and exclusive

    Thread Safety:
        All public methods are thread-safe using an internal master lock.

    Example:
        >>> manager = ConfigurationLockManager()
        >>>
        >>> # Acquire exclusive lock for build
        >>> config_key = ("/path/to/project", "esp32c6", "COM3")
        >>> if manager.acquire_exclusive(config_key, "client-123", "Building firmware"):
        ...     try:
        ...         do_build()
        ...     finally:
        ...         manager.release(config_key, "client-123")
        >>>
        >>> # Acquire shared read lock for monitoring
        >>> if manager.acquire_shared_read(config_key, "client-456", "Monitoring output"):
        ...     try:
        ...         read_serial_output()
        ...     finally:
        ...         manager.release(config_key, "client-456")
        >>>
        >>> # Auto-release all locks when client disconnects
        >>> manager.release_all_client_locks("client-123")
    """

    def __init__(self) -> None:
        """Initialize the ConfigurationLockManager."""
        self._master_lock = threading.Lock()  # Protects all internal state
        self._config_locks: dict[tuple[str, str, str], ConfigurationLock] = {}
        self._client_locks: dict[str, set[tuple[str, str, str]]] = {}  # client_id -> config_keys

    def _get_or_create_lock(self, config_key: tuple[str, str, str]) -> ConfigurationLock:
        """Get or create a lock for the given configuration.

        Must be called with _master_lock held.

        Args:
            config_key: Tuple of (project_dir, environment, port)

        Returns:
            ConfigurationLock for the configuration
        """
        if config_key not in self._config_locks:
            self._config_locks[config_key] = ConfigurationLock(config_key=config_key)
        return self._config_locks[config_key]

    def _track_client_lock(self, client_id: str, config_key: tuple[str, str, str]) -> None:
        """Track that a client holds a lock on a configuration.

        Must be called with _master_lock held.

        Args:
            client_id: UUID string identifying the client
            config_key: Configuration key tuple
        """
        if client_id not in self._client_locks:
            self._client_locks[client_id] = set()
        self._client_locks[client_id].add(config_key)

    def _untrack_client_lock(self, client_id: str, config_key: tuple[str, str, str]) -> None:
        """Stop tracking that a client holds a lock on a configuration.

        Must be called with _master_lock held.

        Args:
            client_id: UUID string identifying the client
            config_key: Configuration key tuple
        """
        if client_id in self._client_locks:
            self._client_locks[client_id].discard(config_key)
            if not self._client_locks[client_id]:
                del self._client_locks[client_id]

    def _grant_next_waiting(self, lock: ConfigurationLock) -> None:
        """Grant the exclusive lock to the next waiting client if available.

        Must be called with _master_lock held and lock in UNLOCKED state.

        Args:
            lock: The configuration lock to process
        """
        while lock.waiting_queue:
            waiting = lock.waiting_queue.popleft()
            # Signal the waiting client
            waiting.event.set()
            # Note: The waiting client will acquire the lock when it wakes up
            # We break here to let only one client proceed
            break

    def acquire_exclusive(
        self,
        config_key: tuple[str, str, str],
        client_id: str,
        description: str = "",
        timeout: float = DEFAULT_EXCLUSIVE_TIMEOUT,
    ) -> bool:
        """Acquire an exclusive lock for a configuration.

        An exclusive lock can only be held by one client at a time. It cannot
        be acquired if there are shared read locks or another exclusive lock.

        Args:
            config_key: Tuple of (project_dir, environment, port)
            client_id: UUID string identifying the client
            description: Human-readable description of the operation
            timeout: Maximum time in seconds to wait for lock (0 for non-blocking)

        Returns:
            True if lock was acquired, False if timeout or not available

        Note:
            If timeout > 0 and lock is not immediately available, the client
            will be added to a waiting queue and will be notified when the
            lock becomes available.
        """
        start_time = time.time()
        waiting_request: WaitingRequest | None = None

        with self._master_lock:
            lock = self._get_or_create_lock(config_key)

            # Check if already held by this client (reentrant case)
            if lock.state == LockState.LOCKED_EXCLUSIVE and lock.exclusive_holder and lock.exclusive_holder.client_id == client_id:
                logging.debug(f"Client {client_id} already holds exclusive lock for {config_key}")
                return True

            # Try to acquire immediately if unlocked
            if lock.state == LockState.UNLOCKED:
                lock.state = LockState.LOCKED_EXCLUSIVE
                lock.exclusive_holder = LockHolder(
                    client_id=client_id,
                    description=description,
                    lock_type="exclusive",
                )
                lock.last_activity_at = time.time()
                self._track_client_lock(client_id, config_key)
                logging.debug(f"Exclusive lock acquired for {config_key} by {client_id}")
                return True

            # If non-blocking, return False
            if timeout <= 0:
                logging.debug(f"Exclusive lock not available for {config_key}, current state: {lock.state.value}")
                return False

            # Add to waiting queue
            waiting_request = WaitingRequest(
                client_id=client_id,
                description=description,
            )
            lock.waiting_queue.append(waiting_request)
            logging.debug(f"Client {client_id} added to waiting queue for {config_key}, position: {len(lock.waiting_queue)}")

        # Wait outside the master lock to avoid blocking other operations
        if waiting_request:
            remaining_timeout = timeout - (time.time() - start_time)
            if remaining_timeout > 0:
                signaled = waiting_request.event.wait(timeout=remaining_timeout)
            else:
                signaled = False

            # Try to acquire the lock now
            with self._master_lock:
                lock = self._get_or_create_lock(config_key)

                # Remove from waiting queue if still there
                try:
                    lock.waiting_queue.remove(waiting_request)
                except ValueError:
                    pass  # Already removed

                if not signaled:
                    logging.debug(f"Timeout waiting for exclusive lock on {config_key} for client {client_id}")
                    return False

                # Try to acquire now that we've been signaled
                if lock.state == LockState.UNLOCKED:
                    lock.state = LockState.LOCKED_EXCLUSIVE
                    lock.exclusive_holder = LockHolder(
                        client_id=client_id,
                        description=description,
                        lock_type="exclusive",
                    )
                    lock.last_activity_at = time.time()
                    self._track_client_lock(client_id, config_key)
                    logging.debug(f"Exclusive lock acquired (after wait) for {config_key} by {client_id}")
                    return True
                else:
                    # Lock was taken by someone else
                    logging.debug(f"Lock taken by another client while {client_id} was waiting")
                    return False

        return False

    def acquire_shared_read(
        self,
        config_key: tuple[str, str, str],
        client_id: str,
        description: str = "",
    ) -> bool:
        """Acquire a shared read lock for a configuration.

        Multiple clients can hold shared read locks simultaneously.
        Shared read locks cannot be acquired if there is an exclusive lock
        or if there are clients waiting for an exclusive lock.

        Args:
            config_key: Tuple of (project_dir, environment, port)
            client_id: UUID string identifying the client
            description: Human-readable description of the operation

        Returns:
            True if lock was acquired, False if not available
        """
        with self._master_lock:
            lock = self._get_or_create_lock(config_key)

            # Check if already held by this client
            if client_id in lock.shared_holders:
                logging.debug(f"Client {client_id} already holds shared read lock for {config_key}")
                return True

            # Cannot acquire if exclusive lock is held
            if lock.state == LockState.LOCKED_EXCLUSIVE:
                logging.debug(f"Shared read lock not available for {config_key}, exclusive lock held by {lock.exclusive_holder.client_id if lock.exclusive_holder else 'unknown'}")
                return False

            # Cannot acquire if there are clients waiting for exclusive lock
            # (to prevent starvation of exclusive lock requests)
            if lock.waiting_queue:
                logging.debug(f"Shared read lock not available for {config_key}, {len(lock.waiting_queue)} clients waiting for exclusive lock")
                return False

            # Acquire shared read lock
            lock.state = LockState.LOCKED_SHARED_READ
            lock.shared_holders[client_id] = LockHolder(
                client_id=client_id,
                description=description,
                lock_type="shared_read",
            )
            lock.last_activity_at = time.time()
            self._track_client_lock(client_id, config_key)
            logging.debug(f"Shared read lock acquired for {config_key} by {client_id}, total shared holders: {len(lock.shared_holders)}")
            return True

    def release(self, config_key: tuple[str, str, str], client_id: str) -> bool:
        """Release a lock held by a client.

        This releases either an exclusive lock or a shared read lock,
        depending on what the client holds.

        Args:
            config_key: Tuple of (project_dir, environment, port)
            client_id: UUID string identifying the client

        Returns:
            True if a lock was released, False if client didn't hold a lock
        """
        with self._master_lock:
            if config_key not in self._config_locks:
                logging.debug(f"No lock exists for {config_key} to release")
                return False

            lock = self._config_locks[config_key]

            # Check for exclusive lock
            if lock.state == LockState.LOCKED_EXCLUSIVE and lock.exclusive_holder and lock.exclusive_holder.client_id == client_id:
                lock.state = LockState.UNLOCKED
                lock.exclusive_holder = None
                lock.last_activity_at = time.time()
                self._untrack_client_lock(client_id, config_key)
                logging.debug(f"Exclusive lock released for {config_key} by {client_id}")
                # Grant to next waiting client if any
                self._grant_next_waiting(lock)
                return True

            # Check for shared read lock
            if client_id in lock.shared_holders:
                del lock.shared_holders[client_id]
                lock.last_activity_at = time.time()
                self._untrack_client_lock(client_id, config_key)

                # Update state if no more shared holders
                if not lock.shared_holders:
                    lock.state = LockState.UNLOCKED
                    # Grant to next waiting client if any
                    self._grant_next_waiting(lock)

                logging.debug(f"Shared read lock released for {config_key} by {client_id}, remaining shared holders: {len(lock.shared_holders)}")
                return True

            logging.debug(f"Client {client_id} does not hold a lock for {config_key}")
            return False

    def release_all_client_locks(self, client_id: str) -> int:
        """Release all locks held by a client.

        This should be called when a client disconnects to ensure
        all its locks are properly released.

        Args:
            client_id: UUID string identifying the client

        Returns:
            Number of locks released
        """
        released_count = 0

        with self._master_lock:
            # Get copy of config keys since we'll be modifying the set
            config_keys = list(self._client_locks.get(client_id, set()))

        # Release each lock (release() will acquire _master_lock internally)
        for config_key in config_keys:
            if self.release(config_key, client_id):
                released_count += 1

        if released_count > 0:
            logging.info(f"Released {released_count} locks for disconnected client {client_id}")

        return released_count

    def get_lock_status(self, config_key: tuple[str, str, str]) -> dict[str, Any]:
        """Get detailed status of a specific configuration lock.

        Args:
            config_key: Tuple of (project_dir, environment, port)

        Returns:
            Dictionary with lock state, holders, waiting queue, etc.
        """
        with self._master_lock:
            if config_key not in self._config_locks:
                return {
                    "config_key": {
                        "project_dir": config_key[0],
                        "environment": config_key[1],
                        "port": config_key[2],
                    },
                    "state": LockState.UNLOCKED.value,
                    "exclusive_holder": None,
                    "shared_holders": {},
                    "waiting_queue": [],
                    "waiting_count": 0,
                    "holder_count": 0,
                    "exists": False,
                }

            lock = self._config_locks[config_key]
            result = lock.to_dict()
            result["exists"] = True
            return result

    def get_all_locks(self) -> dict[str, Any]:
        """Get status of all configuration locks.

        Returns:
            Dictionary with all lock information, keyed by string representation
            of config_key.
        """
        with self._master_lock:
            result: dict[str, Any] = {
                "locks": {},
                "total_locks": len(self._config_locks),
                "total_clients": len(self._client_locks),
                "summary": {
                    "unlocked": 0,
                    "exclusive": 0,
                    "shared_read": 0,
                    "waiting_total": 0,
                },
            }

            for config_key, lock in self._config_locks.items():
                key_str = f"{config_key[0]}|{config_key[1]}|{config_key[2]}"
                result["locks"][key_str] = lock.to_dict()

                # Update summary
                if lock.state == LockState.UNLOCKED:
                    result["summary"]["unlocked"] += 1
                elif lock.state == LockState.LOCKED_EXCLUSIVE:
                    result["summary"]["exclusive"] += 1
                elif lock.state == LockState.LOCKED_SHARED_READ:
                    result["summary"]["shared_read"] += 1

                result["summary"]["waiting_total"] += len(lock.waiting_queue)

            return result

    def is_available_for_exclusive(self, config_key: tuple[str, str, str]) -> bool:
        """Check if exclusive lock can be immediately acquired.

        Args:
            config_key: Tuple of (project_dir, environment, port)

        Returns:
            True if exclusive lock is immediately available, False otherwise
        """
        with self._master_lock:
            if config_key not in self._config_locks:
                return True

            lock = self._config_locks[config_key]
            return lock.state == LockState.UNLOCKED

    def upgrade_to_exclusive(
        self,
        config_key: tuple[str, str, str],
        client_id: str,
        timeout: float = DEFAULT_EXCLUSIVE_TIMEOUT,
    ) -> bool:
        """Upgrade a shared read lock to an exclusive lock.

        The client must already hold a shared read lock on the configuration.
        The upgrade will wait for other shared readers to release their locks.

        Args:
            config_key: Tuple of (project_dir, environment, port)
            client_id: UUID string identifying the client
            timeout: Maximum time in seconds to wait for exclusive access

        Returns:
            True if lock was upgraded, False if upgrade failed or timed out
        """
        start_time = time.time()
        waiting_request: WaitingRequest | None = None

        with self._master_lock:
            if config_key not in self._config_locks:
                logging.debug(f"No lock exists for {config_key} to upgrade")
                return False

            lock = self._config_locks[config_key]

            # Client must hold a shared read lock
            if client_id not in lock.shared_holders:
                logging.debug(f"Client {client_id} does not hold shared read lock for {config_key}")
                return False

            # If this is the only shared holder, upgrade immediately
            if len(lock.shared_holders) == 1 and not lock.waiting_queue:
                del lock.shared_holders[client_id]
                lock.state = LockState.LOCKED_EXCLUSIVE
                lock.exclusive_holder = LockHolder(
                    client_id=client_id,
                    description=f"Upgraded from shared: {lock.shared_holders.get(client_id, LockHolder(client_id=client_id, description='')).description}",
                    lock_type="exclusive",
                )
                lock.last_activity_at = time.time()
                logging.debug(f"Lock upgraded to exclusive for {config_key} by {client_id}")
                return True

            # Need to wait for other shared holders to release
            if timeout <= 0:
                logging.debug(f"Cannot upgrade lock for {config_key}, {len(lock.shared_holders) - 1} other shared holders")
                return False

            # Release our shared lock and join waiting queue with priority
            # We add to front of queue for upgrade requests
            del lock.shared_holders[client_id]
            if not lock.shared_holders:
                lock.state = LockState.UNLOCKED

            waiting_request = WaitingRequest(
                client_id=client_id,
                description="Upgrading from shared read lock",
            )
            # Add to front of queue for upgrades (priority)
            lock.waiting_queue.appendleft(waiting_request)
            lock.last_activity_at = time.time()
            logging.debug(f"Client {client_id} waiting for upgrade on {config_key}")

        # Wait outside the master lock
        if waiting_request:
            # Grant immediately if lock is now unlocked
            with self._master_lock:
                lock_check = self._config_locks.get(config_key)
                if lock_check and lock_check.state == LockState.UNLOCKED:
                    try:
                        lock_check.waiting_queue.remove(waiting_request)
                    except ValueError:
                        pass
                    lock_check.state = LockState.LOCKED_EXCLUSIVE
                    lock_check.exclusive_holder = LockHolder(
                        client_id=client_id,
                        description="Upgraded from shared read lock",
                        lock_type="exclusive",
                    )
                    lock_check.last_activity_at = time.time()
                    # Don't re-track since we kept the tracking from shared
                    logging.debug(f"Lock upgraded (immediate) to exclusive for {config_key} by {client_id}")
                    return True

            remaining_timeout = timeout - (time.time() - start_time)
            if remaining_timeout > 0:
                signaled = waiting_request.event.wait(timeout=remaining_timeout)
            else:
                signaled = False

            with self._master_lock:
                lock_wait = self._config_locks.get(config_key)
                if not lock_wait:
                    return False

                # Remove from waiting queue if still there
                try:
                    lock_wait.waiting_queue.remove(waiting_request)
                except ValueError:
                    pass

                if not signaled:
                    logging.debug(f"Timeout waiting for upgrade on {config_key} for client {client_id}")
                    # Re-acquire shared lock
                    lock_wait.shared_holders[client_id] = LockHolder(
                        client_id=client_id,
                        description="Upgrade timeout - restored shared",
                        lock_type="shared_read",
                    )
                    if lock_wait.state == LockState.UNLOCKED:
                        lock_wait.state = LockState.LOCKED_SHARED_READ
                    return False

                # Try to acquire exclusive now
                if lock_wait.state == LockState.UNLOCKED:
                    lock_wait.state = LockState.LOCKED_EXCLUSIVE
                    lock_wait.exclusive_holder = LockHolder(
                        client_id=client_id,
                        description="Upgraded from shared read lock",
                        lock_type="exclusive",
                    )
                    lock_wait.last_activity_at = time.time()
                    logging.debug(f"Lock upgraded (after wait) to exclusive for {config_key} by {client_id}")
                    return True
                else:
                    # Someone else got the lock
                    # Re-acquire shared lock
                    if lock_wait.state == LockState.LOCKED_SHARED_READ:
                        lock_wait.shared_holders[client_id] = LockHolder(
                            client_id=client_id,
                            description="Upgrade failed - restored shared",
                            lock_type="shared_read",
                        )
                    return False

        return False

    def downgrade_to_shared(
        self,
        config_key: tuple[str, str, str],
        client_id: str,
    ) -> bool:
        """Downgrade an exclusive lock to a shared read lock.

        The client must hold an exclusive lock on the configuration.
        This allows other readers to acquire shared read locks.

        Args:
            config_key: Tuple of (project_dir, environment, port)
            client_id: UUID string identifying the client

        Returns:
            True if lock was downgraded, False if client didn't hold exclusive lock
        """
        with self._master_lock:
            if config_key not in self._config_locks:
                logging.debug(f"No lock exists for {config_key} to downgrade")
                return False

            lock = self._config_locks[config_key]

            # Client must hold exclusive lock
            if lock.state != LockState.LOCKED_EXCLUSIVE or not lock.exclusive_holder or lock.exclusive_holder.client_id != client_id:
                logging.debug(f"Client {client_id} does not hold exclusive lock for {config_key}")
                return False

            # If there are clients waiting for exclusive, don't downgrade
            # (they should get the exclusive lock next)
            if lock.waiting_queue:
                logging.debug(f"Cannot downgrade lock for {config_key}, {len(lock.waiting_queue)} clients waiting for exclusive")
                return False

            # Downgrade to shared read
            old_description = lock.exclusive_holder.description
            lock.exclusive_holder = None
            lock.state = LockState.LOCKED_SHARED_READ
            lock.shared_holders[client_id] = LockHolder(
                client_id=client_id,
                description=f"Downgraded from exclusive: {old_description}",
                lock_type="shared_read",
            )
            lock.last_activity_at = time.time()
            logging.debug(f"Lock downgraded to shared read for {config_key} by {client_id}")
            return True

    def get_client_locks(self, client_id: str) -> list[dict[str, Any]]:
        """Get all locks held by a specific client.

        Args:
            client_id: UUID string identifying the client

        Returns:
            List of lock information dictionaries
        """
        with self._master_lock:
            config_keys = self._client_locks.get(client_id, set())
            result = []

            for config_key in config_keys:
                if config_key in self._config_locks:
                    lock = self._config_locks[config_key]
                    lock_type = None

                    if lock.state == LockState.LOCKED_EXCLUSIVE and lock.exclusive_holder and lock.exclusive_holder.client_id == client_id:
                        lock_type = "exclusive"
                    elif client_id in lock.shared_holders:
                        lock_type = "shared_read"

                    if lock_type:
                        result.append(
                            {
                                "config_key": {
                                    "project_dir": config_key[0],
                                    "environment": config_key[1],
                                    "port": config_key[2],
                                },
                                "lock_type": lock_type,
                                "state": lock.state.value,
                            }
                        )

            return result

    def cleanup_unused_locks(self, older_than: float = 3600.0) -> int:
        """Clean up locks that haven't been used recently.

        This removes lock entries that are:
        - Not currently held (UNLOCKED state)
        - Have not had activity in the specified time period

        Args:
            older_than: Time in seconds. Locks inactive longer than this are removed.

        Returns:
            Number of locks removed
        """
        current_time = time.time()
        removed_count = 0

        with self._master_lock:
            keys_to_remove = []

            for config_key, lock in self._config_locks.items():
                if lock.is_held():
                    continue  # Don't remove held locks

                if current_time - lock.last_activity_at > older_than:
                    keys_to_remove.append(config_key)

            for config_key in keys_to_remove:
                del self._config_locks[config_key]
                removed_count += 1
                logging.debug(f"Cleaned up unused configuration lock: {config_key}")

        if removed_count > 0:
            logging.info(f"Cleaned up {removed_count} unused configuration locks")

        return removed_count

    def clear_all_locks(self) -> int:
        """Clear all locks (use with extreme caution - only for daemon restart).

        This releases all locks and clears all internal state.
        Should only be used during daemon shutdown/restart.

        Returns:
            Number of locks cleared
        """
        with self._master_lock:
            count = len(self._config_locks)

            # Wake up any waiting clients
            for lock in self._config_locks.values():
                for waiting in lock.waiting_queue:
                    waiting.event.set()

            self._config_locks.clear()
            self._client_locks.clear()

            if count > 0:
                logging.info(f"Cleared all {count} configuration locks")

            return count
