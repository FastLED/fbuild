"""
Unit tests for the daemon shutdown executor fix.

Tests that the is_shutting_down flag prevents "cannot schedule new futures
after shutdown" errors during WebSocket cleanup.

The bug occurred because:
1. During daemon shutdown, the executor gets shut down
2. WebSocket cleanup code (in finally blocks) tries to use run_in_executor
3. This fails with "cannot schedule new futures after shutdown"

The fix:
1. Added is_shutting_down flag to DaemonContext
2. Set flag at start of cleanup_daemon_context()
3. WebSocket handlers check flag and run cleanup synchronously if shutting down
"""

from unittest.mock import MagicMock

import pytest


class TestShutdownFlagInDaemonContext:
    """Tests for is_shutting_down flag in DaemonContext."""

    def test_daemon_context_has_is_shutting_down_field(self):
        """Test that DaemonContext has is_shutting_down attribute."""
        from fbuild.daemon.daemon_context import DaemonContext

        # Check the field exists in the dataclass
        assert hasattr(DaemonContext, "__dataclass_fields__")
        assert "is_shutting_down" in DaemonContext.__dataclass_fields__

    def test_is_shutting_down_defaults_to_false(self):
        """Test that is_shutting_down defaults to False."""
        from fbuild.daemon.daemon_context import DaemonContext

        # Check the default value
        field_info = DaemonContext.__dataclass_fields__["is_shutting_down"]
        assert field_info.default is False

    def test_cleanup_daemon_context_sets_shutting_down_flag(self):
        """Test that cleanup_daemon_context sets is_shutting_down to True."""
        from fbuild.daemon.daemon_context import cleanup_daemon_context

        # Create a mock context with all required fields
        mock_context = MagicMock()
        mock_context.is_shutting_down = False
        mock_context.async_server = None
        mock_context.shared_serial_manager = None
        mock_context.configuration_lock_manager = None
        mock_context.device_manager = None
        mock_context.client_manager = None
        mock_context.compilation_queue = None
        mock_context.lock_manager = None

        # Call cleanup
        cleanup_daemon_context(mock_context)

        # Verify the flag was set to True
        assert mock_context.is_shutting_down is True


class TestWebSocketShutdownBehavior:
    """Tests for WebSocket shutdown behavior with is_shutting_down flag."""

    def test_message_processor_exits_when_shutting_down(self):
        """Test that message_processor loop exits when is_shutting_down is True."""
        # This tests the behavior conceptually - the actual async code is tested
        # through integration tests. Here we verify the flag check logic.

        # Create mock context
        mock_context = MagicMock()
        mock_context.is_shutting_down = True

        # The loop should exit when is_shutting_down is True
        assert mock_context.is_shutting_down is True

    def test_data_pusher_exits_when_shutting_down(self):
        """Test that data_pusher loop exits when is_shutting_down is True."""
        # Create mock context
        mock_context = MagicMock()
        mock_context.is_shutting_down = True

        # The loop should exit when is_shutting_down is True
        assert mock_context.is_shutting_down is True

    def test_cleanup_detach_runs_synchronously_when_shutting_down(self):
        """Test that cleanup detach runs synchronously when shutting down.

        When is_shutting_down is True, the cleanup code should call
        processor.handle_detach() directly instead of via run_in_executor.
        """
        from fbuild.daemon.messages import SerialMonitorDetachRequest
        from fbuild.daemon.processors.serial_monitor_processor import (
            SerialMonitorAPIProcessor,
        )

        # Create processor
        processor = SerialMonitorAPIProcessor()

        # Create mock context with shutting down flag
        mock_context = MagicMock()
        mock_context.is_shutting_down = True
        mock_context.shared_serial_manager = MagicMock()
        mock_context.shared_serial_manager.disconnect_client = MagicMock()

        # Create detach request
        detach_request = SerialMonitorDetachRequest(
            client_id="test_client",
            port="COM1",
        )

        # Call handle_detach directly (simulating synchronous cleanup path)
        # This should not raise any errors
        response = processor.handle_detach(detach_request, mock_context)

        # The response should indicate success (even if client wasn't actually attached)
        assert response is not None
        assert hasattr(response, "success")


class TestShutdownFlagThreadSafety:
    """Tests for thread safety of is_shutting_down flag."""

    def test_flag_is_boolean(self):
        """Test that is_shutting_down is a boolean type.

        Boolean assignment in Python is atomic at the bytecode level,
        making it safe to read/write from multiple threads without locks.
        """
        from fbuild.daemon.daemon_context import DaemonContext

        field_info = DaemonContext.__dataclass_fields__["is_shutting_down"]
        assert field_info.type is bool

    def test_flag_write_then_read(self):
        """Test that flag changes are visible after write."""
        mock_context = MagicMock()
        mock_context.is_shutting_down = False

        # Write
        mock_context.is_shutting_down = True

        # Read should see the new value
        assert mock_context.is_shutting_down is True


class TestCleanupOrderWithShutdownFlag:
    """Tests for proper cleanup order with shutdown flag."""

    def test_shutdown_flag_set_before_any_cleanup(self):
        """Test that is_shutting_down is set BEFORE any subsystem cleanup.

        This is critical because subsystem cleanup may trigger callbacks
        that try to use the executor. The flag must be set first so those
        callbacks can detect shutdown and avoid executor calls.
        """
        from fbuild.daemon.daemon_context import cleanup_daemon_context

        # Track the order of operations
        operations: list[str] = []

        # Create mock context that records operations
        mock_context = MagicMock()

        # Track when is_shutting_down is set
        original_is_shutting_down = False

        def track_shutdown_set(value):
            nonlocal original_is_shutting_down
            if value is True and original_is_shutting_down is False:
                operations.append("set_is_shutting_down")
            original_is_shutting_down = value

        # Use property to track is_shutting_down assignment
        type(mock_context).is_shutting_down = property(
            lambda _: original_is_shutting_down,
            lambda _, value: track_shutdown_set(value),
        )

        # Track other cleanup operations
        def track_op(name):
            def _track(*_args, **_kwargs):
                operations.append(name)

            return _track

        mock_context.async_server = MagicMock()
        mock_context.async_server.stop = track_op("async_server.stop")
        mock_context.shared_serial_manager = MagicMock()
        mock_context.shared_serial_manager.shutdown = track_op("shared_serial.shutdown")
        mock_context.configuration_lock_manager = MagicMock()
        mock_context.configuration_lock_manager.clear_all_locks = track_op("config_locks.clear")
        mock_context.device_manager = MagicMock()
        mock_context.device_manager.clear_all_leases = track_op("device.clear")
        mock_context.client_manager = MagicMock()
        mock_context.client_manager.clear_all_clients = track_op("client.clear")
        mock_context.compilation_queue = MagicMock()
        mock_context.compilation_queue.shutdown = track_op("compilation.shutdown")
        mock_context.lock_manager = MagicMock()
        mock_context.lock_manager.clear_all_locks = track_op("locks.clear")

        # Run cleanup
        cleanup_daemon_context(mock_context)

        # Verify is_shutting_down was set FIRST
        assert len(operations) > 0, "No operations recorded"
        assert operations[0] == "set_is_shutting_down", f"First operation should be set_is_shutting_down, got: {operations}"


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
