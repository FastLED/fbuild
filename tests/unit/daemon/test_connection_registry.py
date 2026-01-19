"""
Unit tests for ConnectionRegistry.

Tests connection lifecycle, slot management, heartbeat tracking, and cleanup.
"""

import threading
import time
import unittest

from fbuild.daemon.connection_registry import (
    ConnectionRegistry,
    ConnectionState,
    PlatformSlot,
)


class TestConnectionState(unittest.TestCase):
    """Test cases for ConnectionState dataclass."""

    def _create_state(self, **kwargs) -> ConnectionState:
        """Create a ConnectionState with default values."""
        defaults = {
            "connection_id": "test-conn-1234",
            "project_dir": "/path/to/project",
            "environment": "esp32dev",
            "platform": "esp32s3",
            "connected_at": time.time(),
            "last_heartbeat": time.time(),
            "firmware_uuid": None,
            "slot_held": None,
            "client_pid": 12345,
            "client_hostname": "localhost",
            "client_version": "1.2.11",
        }
        defaults.update(kwargs)
        return ConnectionState(**defaults)

    def test_to_dict_and_from_dict(self):
        """Test serialization round-trip."""
        state = self._create_state()
        data = state.to_dict()

        # Verify key fields
        self.assertEqual(data["connection_id"], "test-conn-1234")
        self.assertEqual(data["platform"], "esp32s3")

        # Deserialize and verify
        restored = ConnectionState.from_dict(data)
        self.assertEqual(restored.connection_id, state.connection_id)
        self.assertEqual(restored.platform, state.platform)

    def test_is_stale_fresh_connection(self):
        """Test that a fresh connection is not stale."""
        state = self._create_state(last_heartbeat=time.time())
        self.assertFalse(state.is_stale(timeout_seconds=30.0))

    def test_is_stale_old_connection(self):
        """Test that an old connection is stale."""
        old_time = time.time() - 60  # 60 seconds ago
        state = self._create_state(last_heartbeat=old_time)
        self.assertTrue(state.is_stale(timeout_seconds=30.0))

    def test_get_age_seconds(self):
        """Test connection age calculation."""
        start_time = time.time() - 10  # 10 seconds ago
        state = self._create_state(connected_at=start_time)
        age = state.get_age_seconds()
        self.assertGreaterEqual(age, 10)
        self.assertLess(age, 11)

    def test_get_idle_seconds(self):
        """Test idle time calculation."""
        heartbeat_time = time.time() - 5  # 5 seconds ago
        state = self._create_state(last_heartbeat=heartbeat_time)
        idle = state.get_idle_seconds()
        self.assertGreaterEqual(idle, 5)
        self.assertLess(idle, 6)


class TestPlatformSlot(unittest.TestCase):
    """Test cases for PlatformSlot dataclass."""

    def test_default_values(self):
        """Test default slot is free."""
        slot = PlatformSlot(platform="esp32s3")
        self.assertIsNone(slot.current_connection_id)
        self.assertTrue(slot.is_free())

    def test_is_free(self):
        """Test slot availability check."""
        slot = PlatformSlot(platform="esp32s3")
        self.assertTrue(slot.is_free())

        slot.current_connection_id = "conn-1234"
        self.assertFalse(slot.is_free())

    def test_is_held_by(self):
        """Test slot holder check."""
        slot = PlatformSlot(platform="esp32s3", current_connection_id="conn-1234")
        self.assertTrue(slot.is_held_by("conn-1234"))
        self.assertFalse(slot.is_held_by("other-conn"))

    def test_get_lock_duration(self):
        """Test lock duration calculation."""
        slot = PlatformSlot(platform="esp32s3")
        self.assertIsNone(slot.get_lock_duration())

        slot.locked_at = time.time() - 5
        duration = slot.get_lock_duration()
        self.assertIsNotNone(duration)
        self.assertGreaterEqual(duration, 5)
        self.assertLess(duration, 6)

    def test_to_dict_and_from_dict(self):
        """Test serialization round-trip."""
        slot = PlatformSlot(
            platform="esp32s3",
            current_connection_id="conn-1234",
            current_firmware_uuid="fw-5678",
            last_build_hash="abc123",
            locked_at=time.time(),
        )
        data = slot.to_dict()
        restored = PlatformSlot.from_dict(data)

        self.assertEqual(restored.platform, slot.platform)
        self.assertEqual(restored.current_connection_id, slot.current_connection_id)


class TestConnectionRegistry(unittest.TestCase):
    """Test cases for ConnectionRegistry."""

    def setUp(self):
        """Create a fresh registry for each test."""
        self.registry = ConnectionRegistry(heartbeat_timeout=30.0)

    def test_register_connection(self):
        """Test registering a new connection."""
        state = self.registry.register_connection(
            connection_id="conn-1234",
            project_dir="/path/to/project",
            environment="esp32dev",
            platform="esp32s3",
            client_pid=12345,
            client_hostname="localhost",
            client_version="1.2.11",
        )

        self.assertEqual(state.connection_id, "conn-1234")
        self.assertEqual(len(self.registry.connections), 1)

    def test_unregister_connection(self):
        """Test unregistering a connection."""
        self.registry.register_connection(
            connection_id="conn-1234",
            project_dir="/path/to/project",
            environment="esp32dev",
            platform="esp32s3",
            client_pid=12345,
            client_hostname="localhost",
            client_version="1.2.11",
        )

        result = self.registry.unregister_connection("conn-1234")
        self.assertTrue(result)
        self.assertEqual(len(self.registry.connections), 0)

    def test_unregister_unknown_connection(self):
        """Test unregistering a non-existent connection."""
        result = self.registry.unregister_connection("unknown-conn")
        self.assertFalse(result)

    def test_update_heartbeat(self):
        """Test updating connection heartbeat."""
        self.registry.register_connection(
            connection_id="conn-1234",
            project_dir="/path/to/project",
            environment="esp32dev",
            platform="esp32s3",
            client_pid=12345,
            client_hostname="localhost",
            client_version="1.2.11",
        )

        # Wait a bit
        time.sleep(0.1)

        # Update heartbeat
        result = self.registry.update_heartbeat("conn-1234")
        self.assertTrue(result)

        # Check that heartbeat was updated
        conn = self.registry.get_connection("conn-1234")
        self.assertIsNotNone(conn)
        self.assertLess(conn.get_idle_seconds(), 0.1)

    def test_update_heartbeat_unknown_connection(self):
        """Test updating heartbeat for non-existent connection."""
        result = self.registry.update_heartbeat("unknown-conn")
        self.assertFalse(result)

    def test_check_stale_connections(self):
        """Test detecting stale connections."""
        # Create registry with short timeout
        registry = ConnectionRegistry(heartbeat_timeout=0.1)

        # Register connection
        registry.register_connection(
            connection_id="conn-1234",
            project_dir="/path/to/project",
            environment="esp32dev",
            platform="esp32s3",
            client_pid=12345,
            client_hostname="localhost",
            client_version="1.2.11",
        )

        # Wait for connection to become stale
        time.sleep(0.15)

        stale = registry.check_stale_connections()
        self.assertIn("conn-1234", stale)

    def test_cleanup_stale_connections(self):
        """Test cleaning up stale connections."""
        # Create registry with short timeout
        registry = ConnectionRegistry(heartbeat_timeout=0.1)

        # Register connection
        registry.register_connection(
            connection_id="conn-1234",
            project_dir="/path/to/project",
            environment="esp32dev",
            platform="esp32s3",
            client_pid=12345,
            client_hostname="localhost",
            client_version="1.2.11",
        )

        # Acquire slot
        registry.acquire_slot("conn-1234", "esp32s3")

        # Wait for connection to become stale
        time.sleep(0.15)

        # Cleanup
        cleaned = registry.cleanup_stale_connections()
        self.assertEqual(cleaned, 1)
        self.assertEqual(len(registry.connections), 0)

        # Slot should be released
        slot = registry.get_slot_status("esp32s3")
        self.assertIsNotNone(slot)
        self.assertTrue(slot.is_free())

    def test_acquire_slot(self):
        """Test acquiring a platform slot."""
        self.registry.register_connection(
            connection_id="conn-1234",
            project_dir="/path/to/project",
            environment="esp32dev",
            platform="esp32s3",
            client_pid=12345,
            client_hostname="localhost",
            client_version="1.2.11",
        )

        result = self.registry.acquire_slot("conn-1234", "esp32s3")
        self.assertTrue(result)

        slot = self.registry.get_slot_status("esp32s3")
        self.assertIsNotNone(slot)
        self.assertFalse(slot.is_free())
        self.assertTrue(slot.is_held_by("conn-1234"))

    def test_acquire_slot_already_held(self):
        """Test acquiring slot already held by another connection."""
        # Register two connections
        self.registry.register_connection(
            connection_id="conn-1",
            project_dir="/path/to/project",
            environment="esp32dev",
            platform="esp32s3",
            client_pid=12345,
            client_hostname="localhost",
            client_version="1.2.11",
        )
        self.registry.register_connection(
            connection_id="conn-2",
            project_dir="/path/to/other",
            environment="esp32dev",
            platform="esp32s3",
            client_pid=12346,
            client_hostname="localhost",
            client_version="1.2.11",
        )

        # First connection acquires slot
        result1 = self.registry.acquire_slot("conn-1", "esp32s3")
        self.assertTrue(result1)

        # Second connection fails to acquire same slot
        result2 = self.registry.acquire_slot("conn-2", "esp32s3")
        self.assertFalse(result2)

    def test_acquire_same_slot_twice(self):
        """Test acquiring the same slot twice is idempotent."""
        self.registry.register_connection(
            connection_id="conn-1234",
            project_dir="/path/to/project",
            environment="esp32dev",
            platform="esp32s3",
            client_pid=12345,
            client_hostname="localhost",
            client_version="1.2.11",
        )

        result1 = self.registry.acquire_slot("conn-1234", "esp32s3")
        result2 = self.registry.acquire_slot("conn-1234", "esp32s3")
        self.assertTrue(result1)
        self.assertTrue(result2)

    def test_release_slot(self):
        """Test releasing a platform slot."""
        self.registry.register_connection(
            connection_id="conn-1234",
            project_dir="/path/to/project",
            environment="esp32dev",
            platform="esp32s3",
            client_pid=12345,
            client_hostname="localhost",
            client_version="1.2.11",
        )

        self.registry.acquire_slot("conn-1234", "esp32s3")
        result = self.registry.release_slot("conn-1234")
        self.assertTrue(result)

        slot = self.registry.get_slot_status("esp32s3")
        self.assertTrue(slot.is_free())

    def test_release_slot_no_slot_held(self):
        """Test releasing when no slot is held."""
        self.registry.register_connection(
            connection_id="conn-1234",
            project_dir="/path/to/project",
            environment="esp32dev",
            platform="esp32s3",
            client_pid=12345,
            client_hostname="localhost",
            client_version="1.2.11",
        )

        result = self.registry.release_slot("conn-1234")
        self.assertFalse(result)

    def test_set_firmware_uuid(self):
        """Test setting firmware UUID for a connection."""
        self.registry.register_connection(
            connection_id="conn-1234",
            project_dir="/path/to/project",
            environment="esp32dev",
            platform="esp32s3",
            client_pid=12345,
            client_hostname="localhost",
            client_version="1.2.11",
        )

        self.registry.acquire_slot("conn-1234", "esp32s3")
        result = self.registry.set_firmware_uuid("conn-1234", "fw-5678")
        self.assertTrue(result)

        conn = self.registry.get_connection("conn-1234")
        self.assertEqual(conn.firmware_uuid, "fw-5678")

        slot = self.registry.get_slot_status("esp32s3")
        self.assertEqual(slot.current_firmware_uuid, "fw-5678")

    def test_get_all_connections(self):
        """Test getting all connections."""
        self.registry.register_connection(
            connection_id="conn-1",
            project_dir="/path/to/project1",
            environment="esp32dev",
            platform="esp32s3",
            client_pid=12345,
            client_hostname="localhost",
            client_version="1.2.11",
        )
        self.registry.register_connection(
            connection_id="conn-2",
            project_dir="/path/to/project2",
            environment="esp32c6",
            platform="esp32c6",
            client_pid=12346,
            client_hostname="localhost",
            client_version="1.2.11",
        )

        connections = self.registry.get_all_connections()
        self.assertEqual(len(connections), 2)

    def test_get_all_slots(self):
        """Test getting all platform slots."""
        self.registry.register_connection(
            connection_id="conn-1",
            project_dir="/path/to/project",
            environment="esp32dev",
            platform="esp32s3",
            client_pid=12345,
            client_hostname="localhost",
            client_version="1.2.11",
        )

        self.registry.acquire_slot("conn-1", "esp32s3")

        slots = self.registry.get_all_slots()
        self.assertEqual(len(slots), 1)
        self.assertIn("esp32s3", slots)

    def test_release_all_client_resources(self):
        """Test releasing all resources for a connection."""
        self.registry.register_connection(
            connection_id="conn-1234",
            project_dir="/path/to/project",
            environment="esp32dev",
            platform="esp32s3",
            client_pid=12345,
            client_hostname="localhost",
            client_version="1.2.11",
        )

        self.registry.acquire_slot("conn-1234", "esp32s3")
        self.registry.set_firmware_uuid("conn-1234", "fw-5678")

        self.registry.release_all_client_resources("conn-1234")

        conn = self.registry.get_connection("conn-1234")
        self.assertIsNone(conn.firmware_uuid)
        self.assertIsNone(conn.slot_held)

        slot = self.registry.get_slot_status("esp32s3")
        self.assertTrue(slot.is_free())

    def test_to_dict(self):
        """Test converting registry to dictionary."""
        self.registry.register_connection(
            connection_id="conn-1234",
            project_dir="/path/to/project",
            environment="esp32dev",
            platform="esp32s3",
            client_pid=12345,
            client_hostname="localhost",
            client_version="1.2.11",
        )

        self.registry.acquire_slot("conn-1234", "esp32s3")

        data = self.registry.to_dict()
        self.assertEqual(data["connection_count"], 1)
        self.assertEqual(data["slot_count"], 1)
        self.assertEqual(len(data["connections"]), 1)
        self.assertIn("esp32s3", data["platform_slots"])

    def test_re_register_connection_cleans_up_old_state(self):
        """Test that re-registering a connection cleans up old state."""
        # Register and acquire slot
        self.registry.register_connection(
            connection_id="conn-1234",
            project_dir="/path/to/project",
            environment="esp32dev",
            platform="esp32s3",
            client_pid=12345,
            client_hostname="localhost",
            client_version="1.2.11",
        )
        self.registry.acquire_slot("conn-1234", "esp32s3")

        # Re-register same connection
        self.registry.register_connection(
            connection_id="conn-1234",
            project_dir="/path/to/new_project",
            environment="esp32c6",
            platform="esp32c6",
            client_pid=12345,
            client_hostname="localhost",
            client_version="1.2.11",
        )

        # Old slot should be released
        old_slot = self.registry.get_slot_status("esp32s3")
        self.assertIsNotNone(old_slot)
        self.assertTrue(old_slot.is_free())

        # Connection should have new values
        conn = self.registry.get_connection("conn-1234")
        self.assertEqual(conn.project_dir, "/path/to/new_project")
        self.assertEqual(conn.platform, "esp32c6")


class TestConnectionRegistryThreadSafety(unittest.TestCase):
    """Test cases for ConnectionRegistry thread safety."""

    def test_concurrent_registrations(self):
        """Test registering connections from multiple threads."""
        registry = ConnectionRegistry(heartbeat_timeout=30.0)
        errors = []

        def register_connection(i):
            try:
                registry.register_connection(
                    connection_id=f"conn-{i}",
                    project_dir=f"/path/to/project{i}",
                    environment="esp32dev",
                    platform="esp32s3",
                    client_pid=12345 + i,
                    client_hostname="localhost",
                    client_version="1.2.11",
                )
            except Exception as e:
                errors.append(e)

        threads = [threading.Thread(target=register_connection, args=(i,)) for i in range(10)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        self.assertEqual(len(errors), 0)
        self.assertEqual(len(registry.connections), 10)

    def test_concurrent_heartbeats(self):
        """Test updating heartbeats from multiple threads."""
        registry = ConnectionRegistry(heartbeat_timeout=30.0)

        # Register connections
        for i in range(5):
            registry.register_connection(
                connection_id=f"conn-{i}",
                project_dir=f"/path/to/project{i}",
                environment="esp32dev",
                platform="esp32s3",
                client_pid=12345 + i,
                client_hostname="localhost",
                client_version="1.2.11",
            )

        errors = []

        def send_heartbeats(conn_id):
            try:
                for _ in range(100):
                    registry.update_heartbeat(conn_id)
            except Exception as e:
                errors.append(e)

        threads = [threading.Thread(target=send_heartbeats, args=(f"conn-{i}",)) for i in range(5)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        self.assertEqual(len(errors), 0)

    def test_concurrent_slot_acquisition(self):
        """Test acquiring slots from multiple threads."""
        registry = ConnectionRegistry(heartbeat_timeout=30.0)

        # Register connections
        for i in range(5):
            registry.register_connection(
                connection_id=f"conn-{i}",
                project_dir=f"/path/to/project{i}",
                environment="esp32dev",
                platform="esp32s3",
                client_pid=12345 + i,
                client_hostname="localhost",
                client_version="1.2.11",
            )

        successful_acquisitions = []
        lock = threading.Lock()

        def try_acquire(conn_id):
            if registry.acquire_slot(conn_id, "esp32s3"):
                with lock:
                    successful_acquisitions.append(conn_id)

        threads = [threading.Thread(target=try_acquire, args=(f"conn-{i}",)) for i in range(5)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        # Only one connection should have acquired the slot
        self.assertEqual(len(successful_acquisitions), 1)


if __name__ == "__main__":
    unittest.main()
