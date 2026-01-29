"""Unit tests for the SerialMonitor API.

Tests the fbuild.api.SerialMonitor class and related message types.
"""

import json
import tempfile
import time
from pathlib import Path
from unittest.mock import Mock, patch

import pytest

from fbuild.daemon.messages import (
    SerialMonitorAttachRequest,
    SerialMonitorDetachRequest,
    SerialMonitorPollRequest,
    SerialMonitorResponse,
)


class TestSerialMonitorMessages:
    """Test message serialization/deserialization."""

    def test_attach_request_serialization(self):
        """Test SerialMonitorAttachRequest to_dict/from_dict."""
        request = SerialMonitorAttachRequest(
            client_id="test_client_123",
            port="COM13",
            baud_rate=115200,
            open_if_needed=True,
        )

        # Serialize
        data = request.to_dict()
        assert data["client_id"] == "test_client_123"
        assert data["port"] == "COM13"
        assert data["baud_rate"] == 115200
        assert data["open_if_needed"] is True
        assert "timestamp" in data

        # Deserialize
        restored = SerialMonitorAttachRequest.from_dict(data)
        assert restored.client_id == "test_client_123"
        assert restored.port == "COM13"
        assert restored.baud_rate == 115200
        assert restored.open_if_needed is True

    def test_detach_request_serialization(self):
        """Test SerialMonitorDetachRequest to_dict/from_dict."""
        request = SerialMonitorDetachRequest(
            client_id="test_client_456",
            port="COM13",
        )

        # Serialize
        data = request.to_dict()
        assert data["client_id"] == "test_client_456"
        assert data["port"] == "COM13"

        # Deserialize
        restored = SerialMonitorDetachRequest.from_dict(data)
        assert restored.client_id == "test_client_456"
        assert restored.port == "COM13"

    def test_poll_request_serialization(self):
        """Test SerialMonitorPollRequest to_dict/from_dict."""
        request = SerialMonitorPollRequest(
            client_id="test_client_789",
            port="COM13",
            last_index=42,
            max_lines=100,
        )

        # Serialize
        data = request.to_dict()
        assert data["client_id"] == "test_client_789"
        assert data["port"] == "COM13"
        assert data["last_index"] == 42
        assert data["max_lines"] == 100

        # Deserialize
        restored = SerialMonitorPollRequest.from_dict(data)
        assert restored.client_id == "test_client_789"
        assert restored.port == "COM13"
        assert restored.last_index == 42
        assert restored.max_lines == 100

    def test_response_serialization(self):
        """Test SerialMonitorResponse to_dict/from_dict."""
        response = SerialMonitorResponse(
            success=True,
            message="Attached successfully",
            lines=["line1", "line2", "line3"],
            current_index=150,
            is_preempted=False,
            preempted_by=None,
            bytes_written=42,
        )

        # Serialize
        data = response.to_dict()
        assert data["success"] is True
        assert data["message"] == "Attached successfully"
        assert data["lines"] == ["line1", "line2", "line3"]
        assert data["current_index"] == 150
        assert data["is_preempted"] is False
        assert data["preempted_by"] is None
        assert data["bytes_written"] == 42

        # Deserialize
        restored = SerialMonitorResponse.from_dict(data)
        assert restored.success is True
        assert restored.message == "Attached successfully"
        assert restored.lines == ["line1", "line2", "line3"]
        assert restored.current_index == 150
        assert restored.is_preempted is False
        assert restored.preempted_by is None
        assert restored.bytes_written == 42

    def test_response_defaults(self):
        """Test SerialMonitorResponse default values."""
        response = SerialMonitorResponse(success=True, message="OK")

        data = response.to_dict()
        assert data["lines"] == []
        assert data["current_index"] == 0
        assert data["is_preempted"] is False
        assert data["preempted_by"] is None
        assert data["bytes_written"] == 0


class TestSerialMonitorAPI:
    """Test SerialMonitor API class."""

    @pytest.fixture
    def temp_daemon_dir(self):
        """Create temporary daemon directory for testing."""
        with tempfile.TemporaryDirectory() as tmpdir:
            daemon_dir = Path(tmpdir) / "daemon"
            daemon_dir.mkdir()

            # Patch DAEMON_DIR to use temp directory
            with patch("fbuild.api.serial_monitor.DAEMON_DIR", daemon_dir):
                yield daemon_dir

    @pytest.mark.skip(reason="File-based API deprecated - now uses WebSocket")
    def test_serial_monitor_initialization(self):
        """Test SerialMonitor __init__."""
        from fbuild.api import SerialMonitor

        mon = SerialMonitor(port="COM13", baud_rate=115200)

        assert mon.port == "COM13"
        assert mon.baud_rate == 115200
        assert mon.hooks == []
        assert mon.auto_reconnect is True
        assert mon.verbose is False
        assert mon.client_id.startswith("serial_monitor_")
        assert mon._attached is False
        assert mon._last_index == 0

    def test_serial_monitor_with_hooks(self):
        """Test SerialMonitor with custom hooks."""
        from fbuild.api import SerialMonitor

        hook1 = Mock()
        hook2 = Mock()

        mon = SerialMonitor(port="COM13", hooks=[hook1, hook2])
        assert len(mon.hooks) == 2
        assert mon.hooks[0] is hook1
        assert mon.hooks[1] is hook2

    @pytest.mark.skip(reason="File-based API deprecated - now uses WebSocket")
    def test_write_request_file_atomic(self, temp_daemon_dir):
        """Test _write_request_file uses atomic write."""
        from fbuild.api import SerialMonitor

        mon = SerialMonitor(port="COM13")

        request = SerialMonitorAttachRequest(
            client_id=mon.client_id,
            port="COM13",
            baud_rate=115200,
        )

        request_file = temp_daemon_dir / "test_request.json"

        # Write request
        mon._write_request_file(request_file, request)

        # Verify file exists and contains correct data
        assert request_file.exists()

        with open(request_file) as f:
            data = json.load(f)

        assert data["client_id"] == mon.client_id
        assert data["port"] == "COM13"

    @pytest.mark.skip(reason="File-based API deprecated - now uses WebSocket")
    def test_check_preemption_file_exists(self, temp_daemon_dir):
        """Test _check_preemption detects preemption file."""
        from fbuild.api import SerialMonitor

        mon = SerialMonitor(port="COM13")

        # No preemption initially
        assert mon._check_preemption() is False

        # Create preemption file
        preempt_file = temp_daemon_dir / "serial_monitor_preempt_COM13.json"
        with open(preempt_file, "w") as f:
            json.dump({"port": "COM13", "preempted_at": time.time()}, f)

        # Should detect preemption
        assert mon._check_preemption() is True

    @pytest.mark.skip(reason="File-based API deprecated - now uses WebSocket")
    def test_wait_for_response_timeout(self, temp_daemon_dir):
        """Test _wait_for_response handles timeout."""
        from fbuild.api import SerialMonitor

        mon = SerialMonitor(port="COM13")

        # No response file exists - should timeout
        response = mon._wait_for_response(timeout=0.5)
        assert response is None

    @pytest.mark.skip(reason="File-based API deprecated - now uses WebSocket")
    def test_wait_for_response_success(self, temp_daemon_dir):
        """Test _wait_for_response reads response file."""
        from fbuild.api import SerialMonitor

        mon = SerialMonitor(port="COM13")

        # Create response file with per-client naming scheme
        response_data = SerialMonitorResponse(success=True, message="Attached").to_dict()

        # Response file must match the per-client naming pattern used by SerialMonitor
        response_file = temp_daemon_dir / f"serial_monitor_response_{mon.client_id}.json"
        with open(response_file, "w") as f:
            json.dump(response_data, f)

        # Should read response successfully
        response = mon._wait_for_response(timeout=1.0)
        assert response is not None
        assert response.success is True
        assert response.message == "Attached"

        # Response file should be deleted after reading
        assert not response_file.exists()

    def test_monitor_hook_error(self):
        """Test MonitorHookError exception."""
        from fbuild.api.serial_monitor import MonitorHookError

        def bad_hook(line: str):
            raise ValueError("hook failed")

        original_error = ValueError("hook failed")
        error = MonitorHookError(bad_hook, original_error)

        assert error.hook is bad_hook
        assert error.original_error is original_error
        assert "bad_hook" in str(error)

    def test_monitor_preempted_exception(self):
        """Test MonitorPreemptedException."""
        from fbuild.api.serial_monitor import MonitorPreemptedException

        exc = MonitorPreemptedException(port="COM13", preempted_by="deploy_op")

        assert exc.port == "COM13"
        assert exc.preempted_by == "deploy_op"
        assert "COM13" in str(exc)
        assert "deploy_op" in str(exc)


class TestSerialMonitorProcessor:
    """Test SerialMonitorAPIProcessor handlers."""

    @pytest.fixture
    def mock_context(self):
        """Create mock daemon context."""
        context = Mock()
        context.shared_serial_manager = Mock()
        context.client_manager = Mock()
        return context

    def test_handle_attach_port_already_open(self, mock_context):
        """Test attach when port is already open."""
        from fbuild.daemon.processors.serial_monitor_processor import (
            SerialMonitorAPIProcessor,
        )

        processor = SerialMonitorAPIProcessor()

        # Mock port already open
        mock_context.shared_serial_manager.get_session_info.return_value = {"is_open": True}
        mock_context.shared_serial_manager.attach_reader.return_value = True

        request = SerialMonitorAttachRequest(
            client_id="test_123",
            port="COM13",
            baud_rate=115200,
            open_if_needed=True,
        )

        response = processor.handle_attach(request, mock_context)

        assert response.success is True
        assert "existing session" in response.message.lower()
        mock_context.shared_serial_manager.attach_reader.assert_called_once_with("COM13", "test_123")
        mock_context.client_manager.attach_resource.assert_called_once()

    def test_handle_attach_open_new_port(self, mock_context):
        """Test attach opens new port if needed."""
        from fbuild.daemon.processors.serial_monitor_processor import (
            SerialMonitorAPIProcessor,
        )

        processor = SerialMonitorAPIProcessor()

        # Mock port not open
        mock_context.shared_serial_manager.get_session_info.return_value = None
        mock_context.shared_serial_manager.open_port.return_value = True
        mock_context.shared_serial_manager.attach_reader.return_value = True

        request = SerialMonitorAttachRequest(
            client_id="test_456",
            port="COM13",
            baud_rate=115200,
            open_if_needed=True,
        )

        response = processor.handle_attach(request, mock_context)

        assert response.success is True
        assert "opened" in response.message.lower()
        mock_context.shared_serial_manager.open_port.assert_called_once_with(port="COM13", baud_rate=115200, client_id="test_456")

    def test_handle_detach(self, mock_context):
        """Test detach removes reader."""
        from fbuild.daemon.processors.serial_monitor_processor import (
            SerialMonitorAPIProcessor,
        )

        processor = SerialMonitorAPIProcessor()

        mock_context.shared_serial_manager.detach_reader.return_value = True

        request = SerialMonitorDetachRequest(
            client_id="test_789",
            port="COM13",
        )

        response = processor.handle_detach(request, mock_context)

        assert response.success is True
        assert "detached" in response.message.lower()
        mock_context.shared_serial_manager.detach_reader.assert_called_once_with("COM13", "test_789")
        mock_context.client_manager.detach_resource.assert_called_once()

    def test_handle_poll_returns_new_lines(self, mock_context):
        """Test poll returns new lines from buffer."""
        from fbuild.daemon.processors.serial_monitor_processor import (
            SerialMonitorAPIProcessor,
        )

        processor = SerialMonitorAPIProcessor()

        # Mock session with reader attached
        mock_context.shared_serial_manager.get_session_info.return_value = {"reader_client_ids": ["test_999"]}

        # Mock buffer with lines
        all_lines = ["line1", "line2", "line3", "line4", "line5"]
        mock_context.shared_serial_manager.read_buffer.return_value = all_lines

        request = SerialMonitorPollRequest(
            client_id="test_999",
            port="COM13",
            last_index=2,  # Already read first 2 lines
            max_lines=100,
        )

        response = processor.handle_poll(request, mock_context)

        assert response.success is True
        assert response.lines == ["line3", "line4", "line5"]  # New lines only
        assert response.current_index == 5

    def test_handle_poll_no_new_lines(self, mock_context):
        """Test poll when client is caught up."""
        from fbuild.daemon.processors.serial_monitor_processor import (
            SerialMonitorAPIProcessor,
        )

        processor = SerialMonitorAPIProcessor()

        mock_context.shared_serial_manager.get_session_info.return_value = {"reader_client_ids": ["test_888"]}
        mock_context.shared_serial_manager.read_buffer.return_value = ["line1", "line2"]

        request = SerialMonitorPollRequest(
            client_id="test_888",
            port="COM13",
            last_index=2,  # Already at end
            max_lines=100,
        )

        response = processor.handle_poll(request, mock_context)

        assert response.success is True
        assert response.lines == []  # No new lines
        assert response.current_index == 2

    def test_handle_write(self, mock_context):
        """Test write sends data to serial port."""
        import base64

        from fbuild.daemon.messages import SerialWriteRequest
        from fbuild.daemon.processors.serial_monitor_processor import (
            SerialMonitorAPIProcessor,
        )

        processor = SerialMonitorAPIProcessor()

        mock_context.shared_serial_manager.acquire_writer.return_value = True
        mock_context.shared_serial_manager.write.return_value = 10  # 10 bytes written
        mock_context.shared_serial_manager.release_writer.return_value = True

        data = b"test data\n"
        data_b64 = base64.b64encode(data).decode("ascii")

        request = SerialWriteRequest(
            client_id="test_777",
            port="COM13",
            data=data_b64,
            acquire_writer=True,
        )

        response = processor.handle_write(request, mock_context)

        assert response.success is True
        assert response.bytes_written == 10
        mock_context.shared_serial_manager.acquire_writer.assert_called_once()
        mock_context.shared_serial_manager.write.assert_called_once()
        mock_context.shared_serial_manager.release_writer.assert_called_once()


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
