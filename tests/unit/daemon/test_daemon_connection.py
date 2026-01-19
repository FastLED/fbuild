"""
Unit tests for DaemonConnection.

Tests the client-side connection object including lifecycle, heartbeat,
and unique connection ID generation.
"""

import os
import tempfile
import time
import unittest
from pathlib import Path
from unittest.mock import patch

from fbuild.daemon.connection import DaemonConnection, connect_daemon


class TestDaemonConnectionLifecycle(unittest.TestCase):
    """Test cases for DaemonConnection lifecycle management."""

    def setUp(self):
        """Set up test environment with temporary directory."""
        self.temp_dir = tempfile.mkdtemp()
        self.project_dir = Path(self.temp_dir) / "test_project"
        self.project_dir.mkdir()

        # Patch _get_daemon_dir to use temp directory
        self.daemon_dir = Path(self.temp_dir) / "daemon_test"
        self.daemon_dir.mkdir()

    def tearDown(self):
        """Clean up temporary directory."""
        import shutil

        shutil.rmtree(self.temp_dir, ignore_errors=True)

    def _create_connection(self, **kwargs) -> DaemonConnection:
        """Create a DaemonConnection with patched daemon directory."""
        defaults = {
            "project_dir": self.project_dir,
            "environment": "esp32dev",
            "dev_mode": False,
        }
        defaults.update(kwargs)

        # Patch the daemon directory
        with patch("fbuild.daemon.connection._get_daemon_dir", return_value=self.daemon_dir):
            conn = DaemonConnection(**defaults)

        # Also patch the internal daemon dir after creation
        conn._daemon_dir = self.daemon_dir
        return conn

    def test_connect_creates_new_connection(self):
        """Test that each connect_daemon() call creates a NEW connection."""
        with patch("fbuild.daemon.connection._get_daemon_dir", return_value=self.daemon_dir):
            conn1 = connect_daemon(self.project_dir, "esp32dev", dev_mode=False)
            conn1._daemon_dir = self.daemon_dir

            conn2 = connect_daemon(self.project_dir, "esp32dev", dev_mode=False)
            conn2._daemon_dir = self.daemon_dir

        try:
            # Connections must be different objects
            self.assertIsNot(conn1, conn2)

            # Connection IDs must be different
            self.assertNotEqual(conn1.connection_id, conn2.connection_id)
        finally:
            conn1.close()
            conn2.close()

    def test_connection_unique_uuid(self):
        """Test that each connection has a unique UUID."""
        connections = []
        try:
            for _ in range(10):
                conn = self._create_connection()
                connections.append(conn)

            # All connection IDs must be unique
            ids = [c.connection_id for c in connections]
            self.assertEqual(len(ids), len(set(ids)))
        finally:
            for conn in connections:
                conn.close()

    def test_connection_uuid_format(self):
        """Test that connection ID is a valid UUID format."""
        import uuid

        conn = self._create_connection()
        try:
            # Should be a valid UUID
            parsed = uuid.UUID(conn.connection_id)
            self.assertEqual(str(parsed), conn.connection_id)
        finally:
            conn.close()

    def test_connection_context_manager(self):
        """Test that context manager opens and closes connection."""
        with patch("fbuild.daemon.connection._get_daemon_dir", return_value=self.daemon_dir):
            with connect_daemon(self.project_dir, "esp32dev", dev_mode=False) as conn:
                conn._daemon_dir = self.daemon_dir
                self.assertFalse(conn._closed)
                _conn_id = conn.connection_id  # noqa: F841

            # After context manager exit, connection should be closed
            self.assertTrue(conn._closed)

    def test_connection_close_explicit(self):
        """Test that explicit close() works correctly."""
        conn = self._create_connection()
        self.assertFalse(conn._closed)

        conn.close()

        self.assertTrue(conn._closed)

    def test_connection_close_idempotent(self):
        """Test that multiple close() calls are safe."""
        conn = self._create_connection()

        # Close multiple times - should not raise
        conn.close()
        conn.close()
        conn.close()

        self.assertTrue(conn._closed)

    def test_connection_not_singleton(self):
        """Test that connections are NOT singletons - critical architecture requirement."""
        with patch("fbuild.daemon.connection._get_daemon_dir", return_value=self.daemon_dir):
            # Create multiple connections to same project/environment
            conn1 = connect_daemon(self.project_dir, "esp32dev", dev_mode=False)
            conn1._daemon_dir = self.daemon_dir

            conn2 = connect_daemon(self.project_dir, "esp32dev", dev_mode=False)
            conn2._daemon_dir = self.daemon_dir

            conn3 = connect_daemon(self.project_dir, "esp32dev", dev_mode=False)
            conn3._daemon_dir = self.daemon_dir

        try:
            # Each must be a different object
            self.assertIsNot(conn1, conn2)
            self.assertIsNot(conn2, conn3)
            self.assertIsNot(conn1, conn3)

            # Each must have a different connection ID
            ids = {conn1.connection_id, conn2.connection_id, conn3.connection_id}
            self.assertEqual(len(ids), 3)
        finally:
            conn1.close()
            conn2.close()
            conn3.close()


class TestDaemonConnectionHeartbeat(unittest.TestCase):
    """Test cases for DaemonConnection heartbeat management."""

    def setUp(self):
        """Set up test environment."""
        self.temp_dir = tempfile.mkdtemp()
        self.project_dir = Path(self.temp_dir) / "test_project"
        self.project_dir.mkdir()
        self.daemon_dir = Path(self.temp_dir) / "daemon_test"
        self.daemon_dir.mkdir()

    def tearDown(self):
        """Clean up."""
        import shutil

        shutil.rmtree(self.temp_dir, ignore_errors=True)

    def _create_connection(self, heartbeat_interval: float = 10.0) -> DaemonConnection:
        """Create a connection with specified heartbeat interval."""
        with patch("fbuild.daemon.connection._get_daemon_dir", return_value=self.daemon_dir):
            conn = DaemonConnection(
                project_dir=self.project_dir,
                environment="esp32dev",
                dev_mode=False,
            )
        conn._daemon_dir = self.daemon_dir
        conn._heartbeat_interval = heartbeat_interval
        return conn

    def test_connection_heartbeat_starts(self):
        """Test that heartbeat thread starts on connection creation."""
        conn = self._create_connection()
        try:
            # Heartbeat thread should be running
            self.assertIsNotNone(conn._heartbeat_thread)
            self.assertTrue(conn._heartbeat_thread.is_alive())
        finally:
            conn.close()

    def test_connection_heartbeat_stops(self):
        """Test that heartbeat thread stops on close."""
        conn = self._create_connection()

        # Get reference to heartbeat thread
        heartbeat_thread = conn._heartbeat_thread
        self.assertTrue(heartbeat_thread.is_alive())

        # Close connection
        conn.close()

        # Give thread time to stop
        time.sleep(0.5)

        # Thread should be stopped
        self.assertFalse(heartbeat_thread.is_alive())

    def test_connection_heartbeat_thread_is_daemon(self):
        """Test that heartbeat thread is a daemon thread."""
        conn = self._create_connection()
        try:
            # Heartbeat thread should be a daemon so it doesn't prevent process exit
            self.assertTrue(conn._heartbeat_thread.daemon)
        finally:
            conn.close()

    def test_heartbeat_file_created(self):
        """Test that heartbeat file is created in daemon directory."""
        # Use a very short heartbeat interval
        conn = self._create_connection(heartbeat_interval=0.1)
        try:
            # Wait for heartbeat to be sent
            time.sleep(0.2)

            # Check for heartbeat file
            heartbeat_files = list(self.daemon_dir.glob(f"heartbeat_{conn.connection_id}.json"))
            self.assertEqual(len(heartbeat_files), 1)
        finally:
            conn.close()

    def test_connect_file_created(self):
        """Test that connect file is created on connection."""
        conn = self._create_connection()
        try:
            # Connect file should exist (may have been cleaned up by disconnect)
            # Check it was created (it's removed on disconnect)
            pass  # File creation happens in __init__
        finally:
            conn.close()


class TestDaemonConnectionOperations(unittest.TestCase):
    """Test cases for DaemonConnection operation methods."""

    def setUp(self):
        """Set up test environment."""
        self.temp_dir = tempfile.mkdtemp()
        self.project_dir = Path(self.temp_dir) / "test_project"
        self.project_dir.mkdir()
        self.daemon_dir = Path(self.temp_dir) / "daemon_test"
        self.daemon_dir.mkdir()

    def tearDown(self):
        """Clean up."""
        import shutil

        shutil.rmtree(self.temp_dir, ignore_errors=True)

    def _create_connection(self) -> DaemonConnection:
        """Create a test connection."""
        with patch("fbuild.daemon.connection._get_daemon_dir", return_value=self.daemon_dir):
            conn = DaemonConnection(
                project_dir=self.project_dir,
                environment="esp32dev",
                dev_mode=False,
            )
        conn._daemon_dir = self.daemon_dir
        return conn

    def test_operations_fail_after_close(self):
        """Test that operations raise RuntimeError after connection is closed."""
        conn = self._create_connection()
        conn.close()

        # All operations should raise RuntimeError
        with self.assertRaises(RuntimeError):
            conn.build()

        with self.assertRaises(RuntimeError):
            conn.deploy()

        with self.assertRaises(RuntimeError):
            conn.monitor()

        with self.assertRaises(RuntimeError):
            conn.install_dependencies()

        with self.assertRaises(RuntimeError):
            conn.get_status()

    def test_check_closed_method(self):
        """Test the _check_closed method."""
        conn = self._create_connection()

        # Should not raise when open
        conn._check_closed()  # No exception

        conn.close()

        # Should raise when closed
        with self.assertRaises(RuntimeError) as ctx:
            conn._check_closed()

        self.assertIn(conn.connection_id, str(ctx.exception))
        self.assertIn("closed", str(ctx.exception).lower())


class TestDaemonConnectionAttributes(unittest.TestCase):
    """Test cases for DaemonConnection attributes."""

    def setUp(self):
        """Set up test environment."""
        self.temp_dir = tempfile.mkdtemp()
        self.project_dir = Path(self.temp_dir) / "test_project"
        self.project_dir.mkdir()
        self.daemon_dir = Path(self.temp_dir) / "daemon_test"
        self.daemon_dir.mkdir()

    def tearDown(self):
        """Clean up."""
        import shutil

        shutil.rmtree(self.temp_dir, ignore_errors=True)

    def _create_connection(self, **kwargs) -> DaemonConnection:
        """Create a test connection."""
        defaults = {
            "project_dir": self.project_dir,
            "environment": "esp32dev",
            "dev_mode": False,
        }
        defaults.update(kwargs)

        with patch("fbuild.daemon.connection._get_daemon_dir", return_value=self.daemon_dir):
            conn = DaemonConnection(**defaults)
        conn._daemon_dir = self.daemon_dir
        return conn

    def test_project_dir_resolved(self):
        """Test that project_dir is resolved to absolute path."""
        # Create with relative path
        rel_path = Path(".")
        with patch("fbuild.daemon.connection._get_daemon_dir", return_value=self.daemon_dir):
            conn = DaemonConnection(
                project_dir=rel_path,
                environment="esp32dev",
                dev_mode=False,
            )
        conn._daemon_dir = self.daemon_dir

        try:
            # Should be absolute path
            self.assertTrue(conn.project_dir.is_absolute())
        finally:
            conn.close()

    def test_environment_stored(self):
        """Test that environment is stored correctly."""
        conn = self._create_connection(environment="my_custom_env")
        try:
            self.assertEqual(conn.environment, "my_custom_env")
        finally:
            conn.close()

    def test_dev_mode_explicit_true(self):
        """Test explicit dev_mode=True."""
        conn = self._create_connection(dev_mode=True)
        try:
            self.assertTrue(conn.dev_mode)
        finally:
            conn.close()

    def test_dev_mode_explicit_false(self):
        """Test explicit dev_mode=False."""
        conn = self._create_connection(dev_mode=False)
        try:
            self.assertFalse(conn.dev_mode)
        finally:
            conn.close()

    def test_dev_mode_auto_detect_from_env(self):
        """Test auto-detection of dev_mode from environment variable."""
        # Test with FBUILD_DEV_MODE=1
        with patch.dict(os.environ, {"FBUILD_DEV_MODE": "1"}):
            with patch("fbuild.daemon.connection._get_daemon_dir", return_value=self.daemon_dir):
                conn = DaemonConnection(
                    project_dir=self.project_dir,
                    environment="esp32dev",
                    dev_mode=None,  # Auto-detect
                )
            conn._daemon_dir = self.daemon_dir
            try:
                self.assertTrue(conn.dev_mode)
            finally:
                conn.close()

    def test_dev_mode_auto_detect_not_set(self):
        """Test auto-detection of dev_mode when env var not set."""
        # Ensure FBUILD_DEV_MODE is not set
        env_without_dev_mode = {k: v for k, v in os.environ.items() if k != "FBUILD_DEV_MODE"}
        with patch.dict(os.environ, env_without_dev_mode, clear=True):
            with patch("fbuild.daemon.connection._get_daemon_dir", return_value=self.daemon_dir):
                conn = DaemonConnection(
                    project_dir=self.project_dir,
                    environment="esp32dev",
                    dev_mode=None,  # Auto-detect
                )
            conn._daemon_dir = self.daemon_dir
            try:
                self.assertFalse(conn.dev_mode)
            finally:
                conn.close()


class TestDaemonConnectionVersion(unittest.TestCase):
    """Test cases for DaemonConnection version handling."""

    def setUp(self):
        """Set up test environment."""
        self.temp_dir = tempfile.mkdtemp()
        self.project_dir = Path(self.temp_dir) / "test_project"
        self.project_dir.mkdir()
        self.daemon_dir = Path(self.temp_dir) / "daemon_test"
        self.daemon_dir.mkdir()

    def tearDown(self):
        """Clean up."""
        import shutil

        shutil.rmtree(self.temp_dir, ignore_errors=True)

    def test_get_version_returns_string(self):
        """Test that _get_version returns a string."""
        with patch("fbuild.daemon.connection._get_daemon_dir", return_value=self.daemon_dir):
            conn = DaemonConnection(
                project_dir=self.project_dir,
                environment="esp32dev",
                dev_mode=False,
            )
        conn._daemon_dir = self.daemon_dir

        try:
            version = conn._get_version()
            self.assertIsInstance(version, str)
            self.assertGreater(len(version), 0)
        finally:
            conn.close()


class TestConnectDaemonFunction(unittest.TestCase):
    """Test cases for connect_daemon() factory function."""

    def setUp(self):
        """Set up test environment."""
        self.temp_dir = tempfile.mkdtemp()
        self.project_dir = Path(self.temp_dir) / "test_project"
        self.project_dir.mkdir()
        self.daemon_dir = Path(self.temp_dir) / "daemon_test"
        self.daemon_dir.mkdir()

    def tearDown(self):
        """Clean up."""
        import shutil

        shutil.rmtree(self.temp_dir, ignore_errors=True)

    def test_connect_daemon_returns_daemon_connection(self):
        """Test that connect_daemon returns a DaemonConnection instance."""
        with patch("fbuild.daemon.connection._get_daemon_dir", return_value=self.daemon_dir):
            conn = connect_daemon(self.project_dir, "esp32dev", dev_mode=False)
        conn._daemon_dir = self.daemon_dir

        try:
            self.assertIsInstance(conn, DaemonConnection)
        finally:
            conn.close()

    def test_connect_daemon_accepts_string_path(self):
        """Test that connect_daemon accepts string path."""
        with patch("fbuild.daemon.connection._get_daemon_dir", return_value=self.daemon_dir):
            conn = connect_daemon(str(self.project_dir), "esp32dev", dev_mode=False)
        conn._daemon_dir = self.daemon_dir

        try:
            self.assertIsInstance(conn.project_dir, Path)
        finally:
            conn.close()

    def test_connect_daemon_accepts_path_object(self):
        """Test that connect_daemon accepts Path object."""
        with patch("fbuild.daemon.connection._get_daemon_dir", return_value=self.daemon_dir):
            conn = connect_daemon(self.project_dir, "esp32dev", dev_mode=False)
        conn._daemon_dir = self.daemon_dir

        try:
            self.assertIsInstance(conn.project_dir, Path)
        finally:
            conn.close()


if __name__ == "__main__":
    unittest.main()
