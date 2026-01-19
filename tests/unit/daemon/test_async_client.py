"""
Unit tests for ClientConnectionManager.

Tests client lifecycle management including:
- Client registration and unregistration
- Heartbeat mechanism and updates
- Dead client detection (is_client_alive)
- Automatic dead client cleanup
- Resource attachment/detachment tracking
- Cleanup callback invocation
- Unique client ID generation
- Concurrent registration scenarios
- Edge cases (unknown clients, double registration, etc.)
- Message types validation
"""

import threading
import time
import unittest
from dataclasses import dataclass, field
from typing import Any, Callable


# Define the ClientInfo dataclass that will be tested
@dataclass
class ClientInfo:
    """Information about a connected client.

    Attributes:
        client_id: Unique identifier for the client
        pid: Process ID of the client
        connect_time: Unix timestamp when client connected
        last_heartbeat: Unix timestamp of last heartbeat received
        metadata: Optional metadata associated with the client
        attached_resources: Set of resource IDs attached to this client
    """

    client_id: str
    pid: int
    connect_time: float
    last_heartbeat: float
    metadata: dict[str, Any] = field(default_factory=dict)
    attached_resources: set[str] = field(default_factory=set)


class ClientConnectionManager:
    """Manages client connections, heartbeats, and lifecycle.

    Provides:
    - Client registration/unregistration
    - Heartbeat tracking
    - Dead client detection and cleanup
    - Resource attachment tracking
    - Cleanup callbacks on client disconnect
    """

    # Default heartbeat timeout in seconds (client considered dead if no heartbeat)
    DEFAULT_HEARTBEAT_TIMEOUT = 30.0

    # Default cleanup interval in seconds
    DEFAULT_CLEANUP_INTERVAL = 10.0

    def __init__(
        self,
        heartbeat_timeout: float = DEFAULT_HEARTBEAT_TIMEOUT,
        cleanup_interval: float = DEFAULT_CLEANUP_INTERVAL,
    ):
        """Initialize the client connection manager.

        Args:
            heartbeat_timeout: Seconds before client is considered dead
            cleanup_interval: Seconds between automatic cleanup checks
        """
        self._clients: dict[str, ClientInfo] = {}
        self._lock = threading.RLock()
        self._heartbeat_timeout = heartbeat_timeout
        self._cleanup_interval = cleanup_interval
        self._cleanup_callbacks: list[Callable[[ClientInfo], None]] = []
        self._next_client_id = 1
        self._id_lock = threading.Lock()

    def generate_client_id(self) -> str:
        """Generate a unique client ID.

        Returns:
            Unique string identifier for a client
        """
        with self._id_lock:
            client_id = f"client_{self._next_client_id}"
            self._next_client_id += 1
            return client_id

    def register_client(
        self,
        client_id: str | None = None,
        pid: int = 0,
        metadata: dict[str, Any] | None = None,
    ) -> ClientInfo:
        """Register a new client connection.

        Args:
            client_id: Optional client ID (generated if not provided)
            pid: Process ID of the client
            metadata: Optional metadata to associate with client

        Returns:
            ClientInfo object for the registered client

        Raises:
            ValueError: If client_id is already registered
        """
        if client_id is None:
            client_id = self.generate_client_id()

        now = time.time()

        with self._lock:
            if client_id in self._clients:
                raise ValueError(f"Client '{client_id}' is already registered")

            client = ClientInfo(
                client_id=client_id,
                pid=pid,
                connect_time=now,
                last_heartbeat=now,
                metadata=metadata or {},
                attached_resources=set(),
            )
            self._clients[client_id] = client
            return client

    def unregister_client(self, client_id: str) -> bool:
        """Unregister a client connection.

        Args:
            client_id: ID of client to unregister

        Returns:
            True if client was unregistered, False if client not found
        """
        with self._lock:
            if client_id not in self._clients:
                return False

            client = self._clients.pop(client_id)

            # Invoke cleanup callbacks
            for callback in self._cleanup_callbacks:
                try:
                    callback(client)
                except Exception:
                    # Silently ignore callback errors
                    pass

            return True

    def update_heartbeat(self, client_id: str) -> bool:
        """Update the heartbeat timestamp for a client.

        Args:
            client_id: ID of client to update

        Returns:
            True if heartbeat updated, False if client not found
        """
        with self._lock:
            if client_id not in self._clients:
                return False

            self._clients[client_id].last_heartbeat = time.time()
            return True

    def is_client_alive(self, client_id: str) -> bool:
        """Check if a client is alive based on heartbeat timeout.

        Args:
            client_id: ID of client to check

        Returns:
            True if client is alive (heartbeat within timeout),
            False if client is dead or not found
        """
        with self._lock:
            if client_id not in self._clients:
                return False

            client = self._clients[client_id]
            elapsed = time.time() - client.last_heartbeat
            return elapsed < self._heartbeat_timeout

    def get_client(self, client_id: str) -> ClientInfo | None:
        """Get client info by ID.

        Args:
            client_id: ID of client to retrieve

        Returns:
            ClientInfo if found, None otherwise
        """
        with self._lock:
            return self._clients.get(client_id)

    def get_all_clients(self) -> list[ClientInfo]:
        """Get all registered clients.

        Returns:
            List of all ClientInfo objects
        """
        with self._lock:
            return list(self._clients.values())

    def get_dead_clients(self) -> list[ClientInfo]:
        """Get all clients that have exceeded heartbeat timeout.

        Returns:
            List of ClientInfo for dead clients
        """
        with self._lock:
            now = time.time()
            dead = []
            for client in self._clients.values():
                if now - client.last_heartbeat >= self._heartbeat_timeout:
                    dead.append(client)
            return dead

    def cleanup_dead_clients(self) -> int:
        """Remove all dead clients and invoke cleanup callbacks.

        Returns:
            Number of clients cleaned up
        """
        dead_clients = self.get_dead_clients()
        count = 0

        for client in dead_clients:
            if self.unregister_client(client.client_id):
                count += 1

        return count

    def attach_resource(self, client_id: str, resource_id: str) -> bool:
        """Attach a resource to a client.

        Args:
            client_id: ID of client to attach resource to
            resource_id: ID of resource to attach

        Returns:
            True if resource attached, False if client not found
        """
        with self._lock:
            if client_id not in self._clients:
                return False

            self._clients[client_id].attached_resources.add(resource_id)
            return True

    def detach_resource(self, client_id: str, resource_id: str) -> bool:
        """Detach a resource from a client.

        Args:
            client_id: ID of client to detach resource from
            resource_id: ID of resource to detach

        Returns:
            True if resource detached, False if client or resource not found
        """
        with self._lock:
            if client_id not in self._clients:
                return False

            resources = self._clients[client_id].attached_resources
            if resource_id not in resources:
                return False

            resources.discard(resource_id)
            return True

    def get_client_resources(self, client_id: str) -> set[str] | None:
        """Get all resources attached to a client.

        Args:
            client_id: ID of client

        Returns:
            Set of resource IDs, or None if client not found
        """
        with self._lock:
            if client_id not in self._clients:
                return None
            return set(self._clients[client_id].attached_resources)

    def register_cleanup_callback(self, callback: Callable[[ClientInfo], None]) -> None:
        """Register a callback to be called when a client is cleaned up.

        Args:
            callback: Function taking ClientInfo, called on client cleanup
        """
        with self._lock:
            self._cleanup_callbacks.append(callback)

    def unregister_cleanup_callback(self, callback: Callable[[ClientInfo], None]) -> bool:
        """Unregister a cleanup callback.

        Args:
            callback: Previously registered callback function

        Returns:
            True if callback was removed, False if not found
        """
        with self._lock:
            try:
                self._cleanup_callbacks.remove(callback)
                return True
            except ValueError:
                return False

    def get_client_count(self) -> int:
        """Get the number of registered clients.

        Returns:
            Number of currently registered clients
        """
        with self._lock:
            return len(self._clients)

    def clear_all(self) -> int:
        """Clear all clients (with cleanup callbacks).

        Returns:
            Number of clients cleared
        """
        with self._lock:
            count = len(self._clients)
            client_ids = list(self._clients.keys())

        for client_id in client_ids:
            self.unregister_client(client_id)

        return count


# =============================================================================
# MESSAGE TYPES FOR CLIENT-MANAGER COMMUNICATION
# =============================================================================


class MessageType:
    """Message types for client-manager communication."""

    REGISTER = "register"
    UNREGISTER = "unregister"
    HEARTBEAT = "heartbeat"
    ATTACH_RESOURCE = "attach_resource"
    DETACH_RESOURCE = "detach_resource"
    GET_STATUS = "get_status"

    @classmethod
    def is_valid(cls, message_type: str) -> bool:
        """Check if a message type is valid.

        Args:
            message_type: Type string to validate

        Returns:
            True if valid message type, False otherwise
        """
        valid_types = {
            cls.REGISTER,
            cls.UNREGISTER,
            cls.HEARTBEAT,
            cls.ATTACH_RESOURCE,
            cls.DETACH_RESOURCE,
            cls.GET_STATUS,
        }
        return message_type in valid_types


# =============================================================================
# UNIT TESTS
# =============================================================================


class TestClientInfo(unittest.TestCase):
    """Test cases for ClientInfo dataclass."""

    def test_client_info_creation(self):
        """Test basic ClientInfo creation with required fields."""
        now = time.time()
        client = ClientInfo(
            client_id="test_client",
            pid=12345,
            connect_time=now,
            last_heartbeat=now,
        )

        self.assertEqual(client.client_id, "test_client")
        self.assertEqual(client.pid, 12345)
        self.assertEqual(client.connect_time, now)
        self.assertEqual(client.last_heartbeat, now)
        self.assertEqual(client.metadata, {})
        self.assertEqual(client.attached_resources, set())

    def test_client_info_with_metadata(self):
        """Test ClientInfo creation with metadata."""
        now = time.time()
        metadata = {"version": "1.0", "platform": "linux"}
        client = ClientInfo(
            client_id="test_client",
            pid=12345,
            connect_time=now,
            last_heartbeat=now,
            metadata=metadata,
        )

        self.assertEqual(client.metadata, metadata)

    def test_client_info_with_resources(self):
        """Test ClientInfo creation with attached resources."""
        now = time.time()
        resources = {"port:COM3", "project:/path/to/project"}
        client = ClientInfo(
            client_id="test_client",
            pid=12345,
            connect_time=now,
            last_heartbeat=now,
            attached_resources=resources,
        )

        self.assertEqual(client.attached_resources, resources)

    def test_client_info_mutable_defaults(self):
        """Test that mutable defaults are independent per instance."""
        now = time.time()
        client1 = ClientInfo(
            client_id="client1",
            pid=111,
            connect_time=now,
            last_heartbeat=now,
        )
        client2 = ClientInfo(
            client_id="client2",
            pid=222,
            connect_time=now,
            last_heartbeat=now,
        )

        # Modify client1's collections
        client1.metadata["key"] = "value"
        client1.attached_resources.add("resource1")

        # client2's collections should be unaffected
        self.assertNotIn("key", client2.metadata)
        self.assertNotIn("resource1", client2.attached_resources)


class TestClientRegistration(unittest.TestCase):
    """Test cases for client registration and unregistration."""

    def setUp(self):
        """Create a fresh ClientConnectionManager for each test."""
        self.manager = ClientConnectionManager()

    def test_register_client_basic(self):
        """Test basic client registration."""
        client = self.manager.register_client(client_id="test_client", pid=12345)

        self.assertEqual(client.client_id, "test_client")
        self.assertEqual(client.pid, 12345)
        self.assertIsInstance(client.connect_time, float)
        self.assertIsInstance(client.last_heartbeat, float)
        self.assertEqual(self.manager.get_client_count(), 1)

    def test_register_client_auto_id(self):
        """Test client registration with auto-generated ID."""
        client1 = self.manager.register_client(pid=111)
        client2 = self.manager.register_client(pid=222)

        self.assertNotEqual(client1.client_id, client2.client_id)
        self.assertTrue(client1.client_id.startswith("client_"))
        self.assertTrue(client2.client_id.startswith("client_"))
        self.assertEqual(self.manager.get_client_count(), 2)

    def test_register_client_with_metadata(self):
        """Test client registration with metadata."""
        metadata = {"version": "1.0", "platform": "windows"}
        client = self.manager.register_client(
            client_id="test_client",
            pid=12345,
            metadata=metadata,
        )

        self.assertEqual(client.metadata, metadata)

    def test_register_duplicate_client_raises(self):
        """Test that registering a duplicate client ID raises ValueError."""
        self.manager.register_client(client_id="test_client", pid=12345)

        with self.assertRaises(ValueError) as context:
            self.manager.register_client(client_id="test_client", pid=67890)

        self.assertIn("already registered", str(context.exception))

    def test_unregister_client_success(self):
        """Test successful client unregistration."""
        self.manager.register_client(client_id="test_client", pid=12345)
        self.assertEqual(self.manager.get_client_count(), 1)

        result = self.manager.unregister_client("test_client")

        self.assertTrue(result)
        self.assertEqual(self.manager.get_client_count(), 0)
        self.assertIsNone(self.manager.get_client("test_client"))

    def test_unregister_unknown_client(self):
        """Test unregistering a client that doesn't exist."""
        result = self.manager.unregister_client("unknown_client")

        self.assertFalse(result)

    def test_unregister_already_unregistered_client(self):
        """Test unregistering a client twice returns False on second attempt."""
        self.manager.register_client(client_id="test_client", pid=12345)

        result1 = self.manager.unregister_client("test_client")
        result2 = self.manager.unregister_client("test_client")

        self.assertTrue(result1)
        self.assertFalse(result2)

    def test_get_client(self):
        """Test retrieving a client by ID."""
        self.manager.register_client(client_id="test_client", pid=12345)

        client = self.manager.get_client("test_client")

        self.assertIsNotNone(client)
        self.assertEqual(client.client_id, "test_client")
        self.assertEqual(client.pid, 12345)

    def test_get_nonexistent_client(self):
        """Test retrieving a client that doesn't exist."""
        client = self.manager.get_client("nonexistent")

        self.assertIsNone(client)

    def test_get_all_clients(self):
        """Test retrieving all registered clients."""
        self.manager.register_client(client_id="client1", pid=111)
        self.manager.register_client(client_id="client2", pid=222)
        self.manager.register_client(client_id="client3", pid=333)

        clients = self.manager.get_all_clients()

        self.assertEqual(len(clients), 3)
        client_ids = {c.client_id for c in clients}
        self.assertEqual(client_ids, {"client1", "client2", "client3"})


class TestHeartbeat(unittest.TestCase):
    """Test cases for heartbeat mechanism."""

    def setUp(self):
        """Create a fresh ClientConnectionManager for each test."""
        self.manager = ClientConnectionManager(heartbeat_timeout=1.0)

    def test_update_heartbeat_success(self):
        """Test successful heartbeat update."""
        client = self.manager.register_client(client_id="test_client", pid=12345)
        original_heartbeat = client.last_heartbeat

        # Small delay to ensure time difference
        time.sleep(0.01)

        result = self.manager.update_heartbeat("test_client")

        self.assertTrue(result)
        updated_client = self.manager.get_client("test_client")
        self.assertGreater(updated_client.last_heartbeat, original_heartbeat)

    def test_update_heartbeat_unknown_client(self):
        """Test heartbeat update for unknown client returns False."""
        result = self.manager.update_heartbeat("unknown_client")

        self.assertFalse(result)

    def test_is_client_alive_true(self):
        """Test that recently active client is considered alive."""
        self.manager.register_client(client_id="test_client", pid=12345)

        result = self.manager.is_client_alive("test_client")

        self.assertTrue(result)

    def test_is_client_alive_false_after_timeout(self):
        """Test that client is dead after heartbeat timeout."""
        # Use very short timeout for testing
        manager = ClientConnectionManager(heartbeat_timeout=0.05)
        manager.register_client(client_id="test_client", pid=12345)

        # Wait for timeout to expire
        time.sleep(0.1)

        result = manager.is_client_alive("test_client")

        self.assertFalse(result)

    def test_is_client_alive_unknown_client(self):
        """Test is_client_alive returns False for unknown client."""
        result = self.manager.is_client_alive("unknown_client")

        self.assertFalse(result)

    def test_heartbeat_resets_timeout(self):
        """Test that heartbeat resets the timeout clock."""
        manager = ClientConnectionManager(heartbeat_timeout=0.1)
        manager.register_client(client_id="test_client", pid=12345)

        # Wait almost until timeout
        time.sleep(0.07)
        self.assertTrue(manager.is_client_alive("test_client"))

        # Update heartbeat
        manager.update_heartbeat("test_client")

        # Wait again - should still be alive
        time.sleep(0.07)
        self.assertTrue(manager.is_client_alive("test_client"))

        # Wait for full timeout - now should be dead
        time.sleep(0.1)
        self.assertFalse(manager.is_client_alive("test_client"))


class TestDeadClientCleanup(unittest.TestCase):
    """Test cases for dead client detection and cleanup."""

    def setUp(self):
        """Create a fresh ClientConnectionManager with short timeout."""
        self.manager = ClientConnectionManager(heartbeat_timeout=0.05)

    def test_get_dead_clients_none(self):
        """Test get_dead_clients when all clients are alive."""
        self.manager.register_client(client_id="client1", pid=111)
        self.manager.register_client(client_id="client2", pid=222)

        dead = self.manager.get_dead_clients()

        self.assertEqual(len(dead), 0)

    def test_get_dead_clients_some_dead(self):
        """Test get_dead_clients with mix of alive and dead clients."""
        self.manager.register_client(client_id="client1", pid=111)

        # Wait for timeout
        time.sleep(0.1)

        # Register another client (alive)
        self.manager.register_client(client_id="client2", pid=222)

        dead = self.manager.get_dead_clients()

        self.assertEqual(len(dead), 1)
        self.assertEqual(dead[0].client_id, "client1")

    def test_get_dead_clients_all_dead(self):
        """Test get_dead_clients when all clients are dead."""
        self.manager.register_client(client_id="client1", pid=111)
        self.manager.register_client(client_id="client2", pid=222)

        # Wait for timeout
        time.sleep(0.1)

        dead = self.manager.get_dead_clients()

        self.assertEqual(len(dead), 2)

    def test_cleanup_dead_clients(self):
        """Test cleanup_dead_clients removes dead clients."""
        self.manager.register_client(client_id="client1", pid=111)

        # Wait for timeout
        time.sleep(0.1)

        self.manager.register_client(client_id="client2", pid=222)

        self.assertEqual(self.manager.get_client_count(), 2)

        count = self.manager.cleanup_dead_clients()

        self.assertEqual(count, 1)
        self.assertEqual(self.manager.get_client_count(), 1)
        self.assertIsNone(self.manager.get_client("client1"))
        self.assertIsNotNone(self.manager.get_client("client2"))

    def test_cleanup_dead_clients_none_dead(self):
        """Test cleanup_dead_clients when no clients are dead."""
        self.manager.register_client(client_id="client1", pid=111)
        self.manager.register_client(client_id="client2", pid=222)

        count = self.manager.cleanup_dead_clients()

        self.assertEqual(count, 0)
        self.assertEqual(self.manager.get_client_count(), 2)


class TestResourceAttachment(unittest.TestCase):
    """Test cases for resource attachment/detachment tracking."""

    def setUp(self):
        """Create a fresh ClientConnectionManager for each test."""
        self.manager = ClientConnectionManager()

    def test_attach_resource(self):
        """Test attaching a resource to a client."""
        self.manager.register_client(client_id="test_client", pid=12345)

        result = self.manager.attach_resource("test_client", "port:COM3")

        self.assertTrue(result)
        resources = self.manager.get_client_resources("test_client")
        self.assertIn("port:COM3", resources)

    def test_attach_multiple_resources(self):
        """Test attaching multiple resources to a client."""
        self.manager.register_client(client_id="test_client", pid=12345)

        self.manager.attach_resource("test_client", "port:COM3")
        self.manager.attach_resource("test_client", "project:/path/to/project")
        self.manager.attach_resource("test_client", "file:/tmp/lock")

        resources = self.manager.get_client_resources("test_client")
        self.assertEqual(len(resources), 3)
        self.assertIn("port:COM3", resources)
        self.assertIn("project:/path/to/project", resources)
        self.assertIn("file:/tmp/lock", resources)

    def test_attach_resource_unknown_client(self):
        """Test attaching resource to unknown client returns False."""
        result = self.manager.attach_resource("unknown_client", "port:COM3")

        self.assertFalse(result)

    def test_attach_duplicate_resource(self):
        """Test that attaching same resource twice is idempotent."""
        self.manager.register_client(client_id="test_client", pid=12345)

        self.manager.attach_resource("test_client", "port:COM3")
        self.manager.attach_resource("test_client", "port:COM3")  # Duplicate

        resources = self.manager.get_client_resources("test_client")
        self.assertEqual(len(resources), 1)

    def test_detach_resource(self):
        """Test detaching a resource from a client."""
        self.manager.register_client(client_id="test_client", pid=12345)
        self.manager.attach_resource("test_client", "port:COM3")

        result = self.manager.detach_resource("test_client", "port:COM3")

        self.assertTrue(result)
        resources = self.manager.get_client_resources("test_client")
        self.assertNotIn("port:COM3", resources)

    def test_detach_nonexistent_resource(self):
        """Test detaching a resource that isn't attached returns False."""
        self.manager.register_client(client_id="test_client", pid=12345)

        result = self.manager.detach_resource("test_client", "port:COM3")

        self.assertFalse(result)

    def test_detach_resource_unknown_client(self):
        """Test detaching resource from unknown client returns False."""
        result = self.manager.detach_resource("unknown_client", "port:COM3")

        self.assertFalse(result)

    def test_get_client_resources_unknown_client(self):
        """Test getting resources from unknown client returns None."""
        resources = self.manager.get_client_resources("unknown_client")

        self.assertIsNone(resources)

    def test_resources_cleaned_on_unregister(self):
        """Test that resources are accessible until client is unregistered."""
        self.manager.register_client(client_id="test_client", pid=12345)
        self.manager.attach_resource("test_client", "port:COM3")

        self.manager.unregister_client("test_client")

        resources = self.manager.get_client_resources("test_client")
        self.assertIsNone(resources)


class TestCleanupCallbacks(unittest.TestCase):
    """Test cases for cleanup callback invocation."""

    def setUp(self):
        """Create a fresh ClientConnectionManager for each test."""
        self.manager = ClientConnectionManager(heartbeat_timeout=0.05)

    def test_cleanup_callback_on_unregister(self):
        """Test that cleanup callback is invoked on client unregistration."""
        callback_called = []

        def callback(client: ClientInfo):
            callback_called.append(client.client_id)

        self.manager.register_cleanup_callback(callback)
        self.manager.register_client(client_id="test_client", pid=12345)

        self.manager.unregister_client("test_client")

        self.assertEqual(callback_called, ["test_client"])

    def test_multiple_cleanup_callbacks(self):
        """Test that multiple cleanup callbacks are all invoked."""
        callback1_called = []
        callback2_called = []

        def callback1(client: ClientInfo):
            callback1_called.append(client.client_id)

        def callback2(client: ClientInfo):
            callback2_called.append(client.pid)

        self.manager.register_cleanup_callback(callback1)
        self.manager.register_cleanup_callback(callback2)
        self.manager.register_client(client_id="test_client", pid=12345)

        self.manager.unregister_client("test_client")

        self.assertEqual(callback1_called, ["test_client"])
        self.assertEqual(callback2_called, [12345])

    def test_cleanup_callback_on_dead_client_cleanup(self):
        """Test that cleanup callback is invoked during dead client cleanup."""
        callback_called = []

        def callback(client: ClientInfo):
            callback_called.append(client.client_id)

        self.manager.register_cleanup_callback(callback)
        self.manager.register_client(client_id="dead_client", pid=12345)

        # Wait for timeout
        time.sleep(0.1)

        self.manager.cleanup_dead_clients()

        self.assertEqual(callback_called, ["dead_client"])

    def test_cleanup_callback_exception_ignored(self):
        """Test that exceptions in cleanup callbacks are silently ignored."""
        callback_success = []

        def bad_callback(client: ClientInfo):
            raise RuntimeError("Callback error")

        def good_callback(client: ClientInfo):
            callback_success.append(client.client_id)

        self.manager.register_cleanup_callback(bad_callback)
        self.manager.register_cleanup_callback(good_callback)
        self.manager.register_client(client_id="test_client", pid=12345)

        # Should not raise
        self.manager.unregister_client("test_client")

        # Good callback should still be called
        self.assertEqual(callback_success, ["test_client"])

    def test_unregister_cleanup_callback(self):
        """Test unregistering a cleanup callback."""
        callback_called = []

        def callback(client: ClientInfo):
            callback_called.append(client.client_id)

        self.manager.register_cleanup_callback(callback)

        result = self.manager.unregister_cleanup_callback(callback)
        self.assertTrue(result)

        self.manager.register_client(client_id="test_client", pid=12345)
        self.manager.unregister_client("test_client")

        # Callback should not have been called
        self.assertEqual(callback_called, [])

    def test_unregister_nonexistent_callback(self):
        """Test unregistering a callback that wasn't registered."""

        def callback(client: ClientInfo):
            pass

        result = self.manager.unregister_cleanup_callback(callback)

        self.assertFalse(result)


class TestClientIdGeneration(unittest.TestCase):
    """Test cases for unique client ID generation."""

    def setUp(self):
        """Create a fresh ClientConnectionManager for each test."""
        self.manager = ClientConnectionManager()

    def test_generate_unique_ids(self):
        """Test that generated IDs are unique."""
        ids = set()
        for _ in range(100):
            client_id = self.manager.generate_client_id()
            self.assertNotIn(client_id, ids)
            ids.add(client_id)

    def test_id_format(self):
        """Test that generated IDs follow expected format."""
        client_id = self.manager.generate_client_id()

        self.assertTrue(client_id.startswith("client_"))
        # Should have a number suffix
        suffix = client_id.split("_")[1]
        self.assertTrue(suffix.isdigit())

    def test_concurrent_id_generation(self):
        """Test thread-safe ID generation under concurrency."""
        ids = []
        lock = threading.Lock()

        def generate_ids():
            for _ in range(50):
                client_id = self.manager.generate_client_id()
                with lock:
                    ids.append(client_id)

        threads = [threading.Thread(target=generate_ids) for _ in range(10)]

        for t in threads:
            t.start()
        for t in threads:
            t.join()

        # All 500 IDs should be unique
        self.assertEqual(len(ids), 500)
        self.assertEqual(len(set(ids)), 500)


class TestConcurrentRegistration(unittest.TestCase):
    """Test cases for concurrent client registration scenarios."""

    def setUp(self):
        """Create a fresh ClientConnectionManager for each test."""
        self.manager = ClientConnectionManager()

    def test_concurrent_registration(self):
        """Test concurrent registration of multiple clients."""
        registered = []
        lock = threading.Lock()
        errors = []

        def register_client(i: int):
            try:
                client = self.manager.register_client(pid=i)
                with lock:
                    registered.append(client.client_id)
            except Exception as e:
                with lock:
                    errors.append(str(e))

        threads = [threading.Thread(target=register_client, args=(i,)) for i in range(50)]

        for t in threads:
            t.start()
        for t in threads:
            t.join()

        self.assertEqual(len(errors), 0)
        self.assertEqual(len(registered), 50)
        self.assertEqual(self.manager.get_client_count(), 50)

    def test_concurrent_registration_unregistration(self):
        """Test concurrent registration and unregistration."""
        # Pre-register some clients
        for i in range(25):
            self.manager.register_client(client_id=f"pre_client_{i}", pid=i)

        registration_count = [0]
        unregistration_count = [0]
        lock = threading.Lock()

        def register_clients():
            for i in range(25, 50):
                try:
                    self.manager.register_client(client_id=f"client_{i}", pid=i)
                    with lock:
                        registration_count[0] += 1
                except Exception:
                    pass

        def unregister_clients():
            for i in range(25):
                if self.manager.unregister_client(f"pre_client_{i}"):
                    with lock:
                        unregistration_count[0] += 1

        t1 = threading.Thread(target=register_clients)
        t2 = threading.Thread(target=unregister_clients)

        t1.start()
        t2.start()
        t1.join()
        t2.join()

        self.assertEqual(registration_count[0], 25)
        self.assertEqual(unregistration_count[0], 25)
        self.assertEqual(self.manager.get_client_count(), 25)

    def test_concurrent_heartbeat_updates(self):
        """Test concurrent heartbeat updates on the same client."""
        self.manager.register_client(client_id="test_client", pid=12345)

        update_count = [0]
        lock = threading.Lock()

        def update_heartbeat():
            for _ in range(100):
                if self.manager.update_heartbeat("test_client"):
                    with lock:
                        update_count[0] += 1

        threads = [threading.Thread(target=update_heartbeat) for _ in range(10)]

        for t in threads:
            t.start()
        for t in threads:
            t.join()

        self.assertEqual(update_count[0], 1000)


class TestEdgeCases(unittest.TestCase):
    """Test cases for edge cases and unusual scenarios."""

    def setUp(self):
        """Create a fresh ClientConnectionManager for each test."""
        self.manager = ClientConnectionManager()

    def test_empty_manager(self):
        """Test operations on empty manager."""
        self.assertEqual(self.manager.get_client_count(), 0)
        self.assertEqual(self.manager.get_all_clients(), [])
        self.assertEqual(self.manager.get_dead_clients(), [])
        self.assertEqual(self.manager.cleanup_dead_clients(), 0)

    def test_register_with_empty_string_id(self):
        """Test registering client with empty string ID."""
        client = self.manager.register_client(client_id="", pid=12345)

        self.assertEqual(client.client_id, "")
        self.assertIsNotNone(self.manager.get_client(""))

    def test_register_with_special_characters_id(self):
        """Test registering client with special characters in ID."""
        special_id = "client@host:port/path?query=1"
        client = self.manager.register_client(client_id=special_id, pid=12345)

        self.assertEqual(client.client_id, special_id)
        self.assertIsNotNone(self.manager.get_client(special_id))

    def test_register_with_zero_pid(self):
        """Test registering client with PID of 0."""
        client = self.manager.register_client(client_id="test_client", pid=0)

        self.assertEqual(client.pid, 0)

    def test_register_with_negative_pid(self):
        """Test registering client with negative PID."""
        client = self.manager.register_client(client_id="test_client", pid=-1)

        self.assertEqual(client.pid, -1)

    def test_clear_all_empty(self):
        """Test clear_all on empty manager."""
        count = self.manager.clear_all()

        self.assertEqual(count, 0)

    def test_clear_all_with_clients(self):
        """Test clear_all removes all clients."""
        for i in range(5):
            self.manager.register_client(client_id=f"client_{i}", pid=i)

        self.assertEqual(self.manager.get_client_count(), 5)

        count = self.manager.clear_all()

        self.assertEqual(count, 5)
        self.assertEqual(self.manager.get_client_count(), 0)

    def test_clear_all_invokes_callbacks(self):
        """Test that clear_all invokes cleanup callbacks for each client."""
        callback_calls = []

        def callback(client: ClientInfo):
            callback_calls.append(client.client_id)

        self.manager.register_cleanup_callback(callback)

        for i in range(3):
            self.manager.register_client(client_id=f"client_{i}", pid=i)

        self.manager.clear_all()

        self.assertEqual(len(callback_calls), 3)

    def test_double_register_same_id_error(self):
        """Test that registering same ID twice raises appropriate error."""
        self.manager.register_client(client_id="duplicate", pid=111)

        with self.assertRaises(ValueError):
            self.manager.register_client(client_id="duplicate", pid=222)

        # Original registration should still exist
        client = self.manager.get_client("duplicate")
        self.assertEqual(client.pid, 111)

    def test_none_metadata_handling(self):
        """Test that None metadata is converted to empty dict."""
        client = self.manager.register_client(
            client_id="test_client",
            pid=12345,
            metadata=None,
        )

        self.assertEqual(client.metadata, {})
        self.assertIsInstance(client.metadata, dict)


class TestMessageTypes(unittest.TestCase):
    """Test cases for message type validation."""

    def test_valid_message_types(self):
        """Test that all defined message types are valid."""
        self.assertTrue(MessageType.is_valid(MessageType.REGISTER))
        self.assertTrue(MessageType.is_valid(MessageType.UNREGISTER))
        self.assertTrue(MessageType.is_valid(MessageType.HEARTBEAT))
        self.assertTrue(MessageType.is_valid(MessageType.ATTACH_RESOURCE))
        self.assertTrue(MessageType.is_valid(MessageType.DETACH_RESOURCE))
        self.assertTrue(MessageType.is_valid(MessageType.GET_STATUS))

    def test_invalid_message_types(self):
        """Test that invalid message types return False."""
        self.assertFalse(MessageType.is_valid("invalid"))
        self.assertFalse(MessageType.is_valid(""))
        self.assertFalse(MessageType.is_valid("REGISTER"))  # Case sensitive
        self.assertFalse(MessageType.is_valid("Register"))
        self.assertFalse(MessageType.is_valid("connect"))
        self.assertFalse(MessageType.is_valid("disconnect"))

    def test_message_type_constants(self):
        """Test message type constant values."""
        self.assertEqual(MessageType.REGISTER, "register")
        self.assertEqual(MessageType.UNREGISTER, "unregister")
        self.assertEqual(MessageType.HEARTBEAT, "heartbeat")
        self.assertEqual(MessageType.ATTACH_RESOURCE, "attach_resource")
        self.assertEqual(MessageType.DETACH_RESOURCE, "detach_resource")
        self.assertEqual(MessageType.GET_STATUS, "get_status")


class TestManagerConfiguration(unittest.TestCase):
    """Test cases for manager configuration options."""

    def test_default_configuration(self):
        """Test manager with default configuration."""
        manager = ClientConnectionManager()

        self.assertEqual(
            manager._heartbeat_timeout,
            ClientConnectionManager.DEFAULT_HEARTBEAT_TIMEOUT,
        )
        self.assertEqual(
            manager._cleanup_interval,
            ClientConnectionManager.DEFAULT_CLEANUP_INTERVAL,
        )

    def test_custom_heartbeat_timeout(self):
        """Test manager with custom heartbeat timeout."""
        manager = ClientConnectionManager(heartbeat_timeout=60.0)

        self.assertEqual(manager._heartbeat_timeout, 60.0)

    def test_custom_cleanup_interval(self):
        """Test manager with custom cleanup interval."""
        manager = ClientConnectionManager(cleanup_interval=5.0)

        self.assertEqual(manager._cleanup_interval, 5.0)

    def test_very_short_timeout(self):
        """Test manager with very short heartbeat timeout."""
        manager = ClientConnectionManager(heartbeat_timeout=0.01)

        manager.register_client(client_id="test", pid=123)

        # Should be alive immediately
        self.assertTrue(manager.is_client_alive("test"))

        # Should be dead after timeout
        time.sleep(0.02)
        self.assertFalse(manager.is_client_alive("test"))


if __name__ == "__main__":
    unittest.main()
