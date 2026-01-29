"""Unit tests for the SerialMonitor API.

Tests the fbuild.api.SerialMonitor class and related message types.
"""

import asyncio
import json
import time
from unittest.mock import AsyncMock, Mock, patch

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

    def test_serial_monitor_initialization(self):
        """Test SerialMonitor __init__ with WebSocket fields."""
        from fbuild.api import SerialMonitor

        mon = SerialMonitor(port="COM13", baud_rate=115200)

        # Basic fields (unchanged)
        assert mon.port == "COM13"
        assert mon.baud_rate == 115200
        assert mon.hooks == []
        assert mon.auto_reconnect is True
        assert mon.verbose is False

        # WebSocket fields (not initialized until attach)
        assert mon._ws is None
        assert mon._event_loop is None
        assert mon._receiver_task is None

        # Queues initialized
        assert isinstance(mon._line_queue, asyncio.Queue)
        assert isinstance(mon._error_queue, asyncio.Queue)

        # Client ID and state
        assert mon.client_id.startswith("serial_monitor_")
        assert mon._attached is False
        assert mon.last_line == ""

    def test_serial_monitor_with_hooks(self):
        """Test SerialMonitor with custom hooks."""
        from fbuild.api import SerialMonitor

        hook1 = Mock()
        hook2 = Mock()

        mon = SerialMonitor(port="COM13", hooks=[hook1, hook2])
        assert len(mon.hooks) == 2
        assert mon.hooks[0] is hook1
        assert mon.hooks[1] is hook2

    @patch("fbuild.api.serial_monitor.websockets.connect")
    @patch("fbuild.api.serial_monitor.ensure_daemon_running")
    @patch("fbuild.api.serial_monitor.get_daemon_port", return_value=8765)
    def test_websocket_attach_message(self, mock_port, mock_ensure, mock_connect):
        """Test that attach sends properly formatted WebSocket message."""
        from fbuild.api import SerialMonitor

        # Track messages sent
        sent_messages = []

        async def capture_send(data):
            sent_messages.append(json.loads(data))

        # Mock recv: return attached, then timeout (so _detach() doesn't hang)
        recv_count = 0

        async def mock_recv_impl():
            nonlocal recv_count
            recv_count += 1
            if recv_count == 1:
                # Attach response
                return json.dumps({"type": "attached", "success": True, "message": "Attached"})
            # All subsequent recv() calls timeout (simulates no more messages)
            await asyncio.sleep(10)  # Never returns

        mock_ws = AsyncMock()
        mock_ws.send = capture_send
        mock_ws.recv = mock_recv_impl
        mock_ws.close = AsyncMock()

        # Make connect() return a coroutine that resolves to mock_ws
        async def mock_connect_coro(url):
            return mock_ws

        mock_connect.side_effect = mock_connect_coro

        # Test attach
        mon = SerialMonitor(port="COM13", baud_rate=115200)
        mon._attach()

        # Give receiver task time to start
        time.sleep(0.1)

        # Verify message format
        assert len(sent_messages) >= 1
        msg = sent_messages[0]
        assert msg["type"] == "attach"
        assert msg["client_id"] == mon.client_id
        assert msg["port"] == "COM13"
        assert msg["baud_rate"] == 115200
        assert msg["open_if_needed"] is True

        # Detach will timeout waiting for response (caught and logged)
        mon._detach()

    @patch("fbuild.api.serial_monitor.websockets.connect")
    @patch("fbuild.api.serial_monitor.ensure_daemon_running")
    @patch("fbuild.api.serial_monitor.get_daemon_port", return_value=8765)
    def test_preemption_message_handling(self, mock_port, mock_ensure, mock_connect):
        """Test preemption message handling with auto_reconnect=False."""
        from fbuild.api import SerialMonitor
        from fbuild.api.serial_monitor import MonitorPreemptedException

        # Message sequence: attached → preempted → timeout
        messages = [
            json.dumps({"type": "attached", "success": True, "message": "Attached"}),
            json.dumps({"type": "preempted", "preempted_by": "deploy_123", "reason": "deploy"}),
        ]
        msg_index = 0

        async def mock_recv():
            nonlocal msg_index
            if msg_index < len(messages):
                result = messages[msg_index]
                msg_index += 1
                return result
            # Subsequent recv() calls timeout
            await asyncio.sleep(10)

        mock_ws = AsyncMock()
        mock_ws.recv = mock_recv
        mock_ws.send = AsyncMock()
        mock_ws.close = AsyncMock()

        # Make connect() return a coroutine that resolves to mock_ws
        async def mock_connect_coro(url):
            return mock_ws

        mock_connect.side_effect = mock_connect_coro

        # Test with auto_reconnect=False (should raise exception)
        mon = SerialMonitor(port="COM13", auto_reconnect=False)
        mon._attach()
        time.sleep(0.3)  # Let receiver process preemption

        # Should raise MonitorPreemptedException
        with pytest.raises(MonitorPreemptedException) as exc_info:
            for _ in mon.read_lines(timeout=1.0):
                pass

        assert exc_info.value.port == "COM13"
        assert exc_info.value.preempted_by == "deploy_123"

        # Detach will timeout waiting for response
        mon._detach()

    @patch("fbuild.api.serial_monitor.OPERATION_TIMEOUT", 0.5)
    @patch("fbuild.api.serial_monitor.websockets.connect")
    @patch("fbuild.api.serial_monitor.ensure_daemon_running")
    @patch("fbuild.api.serial_monitor.get_daemon_port", return_value=8765)
    def test_websocket_attach_timeout(self, m_port, m_ensure, m_connect):
        """Test attach timeout when daemon doesn't respond."""
        from fbuild.api import SerialMonitor

        # Mock WebSocket that never responds (times out)
        async def mock_recv_timeout():
            await asyncio.sleep(100)  # Never returns

        mock_ws = AsyncMock()
        mock_ws.recv = mock_recv_timeout
        mock_ws.send = AsyncMock()
        mock_ws.close = AsyncMock()

        # Make connect() return a coroutine that resolves to mock_ws
        async def mock_connect_coro(url):
            return mock_ws

        m_connect.side_effect = mock_connect_coro

        # Attach should timeout
        mon = SerialMonitor(port="COM13")
        with pytest.raises(RuntimeError) as exc_info:
            mon._attach()

        # Check error message contains timeout or failed to attach
        error_msg = str(exc_info.value).lower()
        assert "failed to attach" in error_msg or "timeout" in error_msg or "timed out" in error_msg
        assert mon._attached is False

    @patch("fbuild.api.serial_monitor.websockets.connect")
    @patch("fbuild.api.serial_monitor.ensure_daemon_running")
    @patch("fbuild.api.serial_monitor.get_daemon_port", return_value=8765)
    def test_websocket_attach_success(self, mock_port, mock_ensure, mock_connect):
        """Test successful attach via WebSocket."""
        from fbuild.api import SerialMonitor

        recv_count = 0

        async def mock_recv_impl():
            nonlocal recv_count
            recv_count += 1
            if recv_count == 1:
                # Attach response
                return json.dumps({"type": "attached", "success": True, "message": "Attached to COM13"})
            # Subsequent recv() calls timeout
            await asyncio.sleep(10)

        # Mock successful response
        mock_ws = AsyncMock()
        mock_ws.recv = mock_recv_impl
        mock_ws.send = AsyncMock()
        mock_ws.close = AsyncMock()

        # Make connect() return a coroutine that resolves to mock_ws
        async def mock_connect_coro(url):
            return mock_ws

        mock_connect.side_effect = mock_connect_coro

        # Test successful attach
        mon = SerialMonitor(port="COM13", baud_rate=115200)
        mon._attach()

        # Give receiver task time to start
        time.sleep(0.1)

        # Verify state after attach
        assert mon._attached is True
        assert mon._ws is mock_ws
        assert mon._event_loop is not None
        assert mon._receiver_task is not None

        # Verify message was sent
        mock_ws.send.assert_called_once()

        # Detach will timeout waiting for response
        mon._detach()

    @patch("fbuild.api.serial_monitor.websockets.connect")
    @patch("fbuild.api.serial_monitor.ensure_daemon_running")
    @patch("fbuild.api.serial_monitor.get_daemon_port", return_value=8765)
    def test_context_manager_protocol(self, mock_port, mock_ensure, mock_connect):
        """Test context manager calls attach/detach correctly."""
        from fbuild.api import SerialMonitor

        recv_count = 0

        async def mock_recv_impl():
            nonlocal recv_count
            recv_count += 1
            if recv_count == 1:
                # Attach response
                return json.dumps({"type": "attached", "success": True, "message": "OK"})
            # Subsequent recv() calls timeout
            await asyncio.sleep(10)

        mock_ws = AsyncMock()
        mock_ws.recv = mock_recv_impl
        mock_ws.send = AsyncMock()
        mock_ws.close = AsyncMock()

        # Make connect() return a coroutine that resolves to mock_ws
        async def mock_connect_coro(url):
            return mock_ws

        mock_connect.side_effect = mock_connect_coro

        # Use context manager
        with SerialMonitor(port="COM13") as mon:
            time.sleep(0.1)  # Let receiver task start
            assert mon._attached is True

        # Should detach on exit
        assert mon._attached is False
        mock_ws.close.assert_called()

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
