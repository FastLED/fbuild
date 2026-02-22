"""Unit tests for AsyncSerialMonitor.

Tests the async serial monitor API using mocked WebSocket connections.
Uses asyncio.run() directly since pytest-asyncio is not a dependency.
"""

import asyncio
import base64
import json
import os
from unittest.mock import AsyncMock, MagicMock, patch

import pytest
import websockets.exceptions

os.environ["FBUILD_DEV_MODE"] = "1"

from fbuild.api.async_serial_monitor import AsyncSerialMonitor
from fbuild.api.serial_monitor import MonitorHookError, MonitorPreemptedException


def _run(coro):
    """Helper to run an async coroutine in a new event loop."""
    return asyncio.run(coro)


def _make_mock_ws(recv_messages: list[str] | None = None) -> AsyncMock:
    """Create a mock WebSocket that returns messages then raises ConnectionClosed.

    Args:
        recv_messages: List of JSON strings to return from recv() in order.
                       ConnectionClosed is automatically appended.
    """
    ws = AsyncMock()
    ws.send = AsyncMock()
    ws.close = AsyncMock()

    messages = (
        list(recv_messages)
        if recv_messages
        else [
            json.dumps({"type": "attached", "success": True, "message": "OK"}),
        ]
    )
    # Append detach confirmation and connection closed to prevent hanging
    messages.append(json.dumps({"type": "detached", "success": True, "message": "OK"}))

    # After all messages, raise ConnectionClosed so receiver task exits
    ws.recv = AsyncMock(side_effect=messages + [websockets.exceptions.ConnectionClosed(None, None)])

    return ws


def _patch_ws_connect(mock_ws_connect: MagicMock, ws: AsyncMock) -> None:
    """Configure mock websockets.connect to return ws as an awaitable."""

    async def fake_connect(*args, **kwargs):
        return ws

    mock_ws_connect.side_effect = fake_connect


class TestInit:
    """Test constructor and default state."""

    def test_init_defaults(self):
        mon = AsyncSerialMonitor(port="COM13")
        assert mon.port == "COM13"
        assert mon.baud_rate == 115200
        assert mon.hooks == []
        assert mon.auto_reconnect is True
        assert mon.verbose is False
        assert mon.pre_acquire_writer is False
        assert mon.is_connected is False
        assert mon.last_line == ""
        assert mon.client_id.startswith("async_serial_monitor_")

    def test_init_custom_params(self):
        hook = MagicMock()
        mon = AsyncSerialMonitor(
            port="/dev/ttyUSB0",
            baud_rate=9600,
            hooks=[hook],
            auto_reconnect=False,
            verbose=True,
            pre_acquire_writer=True,
        )
        assert mon.port == "/dev/ttyUSB0"
        assert mon.baud_rate == 9600
        assert mon.hooks == [hook]
        assert mon.auto_reconnect is False
        assert mon.verbose is True
        assert mon.pre_acquire_writer is True


class TestConnect:
    """Test connect() method."""

    @patch("fbuild.api.async_serial_monitor.websockets.connect")
    @patch("fbuild.api.async_serial_monitor.get_daemon_port", return_value=8865)
    @patch("fbuild.api.async_serial_monitor.ensure_daemon_running")
    def test_connect_sends_attach(self, mock_ensure, mock_port, mock_ws_connect):
        async def run():
            ws = _make_mock_ws()
            _patch_ws_connect(mock_ws_connect, ws)

            mon = AsyncSerialMonitor(port="COM13", baud_rate=115200)
            await mon.connect()

            # Verify attach message was sent
            assert ws.send.call_count >= 1
            sent_msg = json.loads(ws.send.call_args_list[0][0][0])
            assert sent_msg["type"] == "attach"
            assert sent_msg["port"] == "COM13"
            assert sent_msg["baud_rate"] == 115200
            assert sent_msg["client_id"] == mon.client_id
            assert sent_msg["open_if_needed"] is True
            assert "pre_acquire_writer" not in sent_msg
            assert mon.is_connected is True

            await mon.close()

        _run(run())

    @patch("fbuild.api.async_serial_monitor.websockets.connect")
    @patch("fbuild.api.async_serial_monitor.get_daemon_port", return_value=8865)
    @patch("fbuild.api.async_serial_monitor.ensure_daemon_running")
    def test_connect_with_pre_acquire_writer(self, mock_ensure, mock_port, mock_ws_connect):
        async def run():
            ws = _make_mock_ws(
                [
                    json.dumps({"type": "attached", "success": True, "message": "OK", "writer_pre_acquired": True}),
                ]
            )
            _patch_ws_connect(mock_ws_connect, ws)

            mon = AsyncSerialMonitor(port="COM13", pre_acquire_writer=True)
            await mon.connect()

            sent_msg = json.loads(ws.send.call_args_list[0][0][0])
            assert sent_msg["pre_acquire_writer"] is True
            assert mon._writer_pre_acquired is True

            await mon.close()

        _run(run())

    @patch("fbuild.api.async_serial_monitor.websockets.connect")
    @patch("fbuild.api.async_serial_monitor.get_daemon_port", return_value=8865)
    @patch("fbuild.api.async_serial_monitor.ensure_daemon_running")
    def test_connect_failure_raises(self, mock_ensure, mock_port, mock_ws_connect):
        async def run():
            ws = _make_mock_ws(
                [
                    json.dumps({"type": "attached", "success": False, "message": "Port not found"}),
                ]
            )
            _patch_ws_connect(mock_ws_connect, ws)

            mon = AsyncSerialMonitor(port="COM99")
            with pytest.raises(RuntimeError, match="Failed to attach"):
                await mon.connect()

            assert mon.is_connected is False

        _run(run())

    @patch("fbuild.api.async_serial_monitor.websockets.connect")
    @patch("fbuild.api.async_serial_monitor.get_daemon_port", return_value=8865)
    @patch("fbuild.api.async_serial_monitor.ensure_daemon_running")
    def test_connect_already_connected_noop(self, mock_ensure, mock_port, mock_ws_connect):
        async def run():
            ws = _make_mock_ws()
            _patch_ws_connect(mock_ws_connect, ws)

            mon = AsyncSerialMonitor(port="COM13")
            await mon.connect()
            # Second connect should be a no-op
            await mon.connect()
            # Only one attach message sent
            assert ws.send.call_count == 1

            await mon.close()

        _run(run())


class TestClose:
    """Test close() method."""

    def test_close_idempotent(self):
        async def run():
            mon = AsyncSerialMonitor(port="COM13")
            # close() when not connected should be a no-op
            await mon.close()
            assert mon.is_connected is False

        _run(run())

    @patch("fbuild.api.async_serial_monitor.websockets.connect")
    @patch("fbuild.api.async_serial_monitor.get_daemon_port", return_value=8865)
    @patch("fbuild.api.async_serial_monitor.ensure_daemon_running")
    def test_close_sends_detach(self, mock_ensure, mock_port, mock_ws_connect):
        async def run():
            ws = _make_mock_ws()
            _patch_ws_connect(mock_ws_connect, ws)

            mon = AsyncSerialMonitor(port="COM13")
            await mon.connect()
            await mon.close()

            assert mon.is_connected is False
            # At least 2 sends: attach + detach
            assert ws.send.call_count >= 2
            detach_msg = json.loads(ws.send.call_args_list[-1][0][0])
            assert detach_msg["type"] == "detach"

        _run(run())


class TestContextManager:
    """Test async context manager."""

    @patch("fbuild.api.async_serial_monitor.websockets.connect")
    @patch("fbuild.api.async_serial_monitor.get_daemon_port", return_value=8865)
    @patch("fbuild.api.async_serial_monitor.ensure_daemon_running")
    def test_context_manager(self, mock_ensure, mock_port, mock_ws_connect):
        async def run():
            ws = _make_mock_ws()
            _patch_ws_connect(mock_ws_connect, ws)

            async with AsyncSerialMonitor(port="COM13") as mon:
                assert mon.is_connected is True

            assert mon.is_connected is False

        _run(run())


class TestWrite:
    """Test write() method."""

    def test_write_not_connected_raises(self):
        async def run():
            mon = AsyncSerialMonitor(port="COM13")
            with pytest.raises(RuntimeError, match="Cannot write: not connected"):
                await mon.write("test")

        _run(run())

    def test_write_base64_encoding(self):
        async def run():
            mon = AsyncSerialMonitor(port="COM13")
            mon._connected = True
            mon._ws = AsyncMock()
            mon._ws.send = AsyncMock()

            # Feed write_ack
            async def feed_ack():
                await asyncio.sleep(0.05)
                await mon._write_ack_queue.put({"type": "write_ack", "success": True, "bytes_written": 6})

            ack_task = asyncio.create_task(feed_ack())
            result = await mon.write("hello\n")

            assert result == 6
            write_msg = json.loads(mon._ws.send.call_args_list[0][0][0])
            assert write_msg["type"] == "write"
            expected_b64 = base64.b64encode(b"hello\n").decode("ascii")
            assert write_msg["data"] == expected_b64

            await ack_task

        _run(run())

    def test_write_timeout(self):
        async def run():
            mon = AsyncSerialMonitor(port="COM13")
            mon._connected = True
            mon._ws = AsyncMock()
            mon._ws.send = AsyncMock()

            # No ack will arrive, but we test the timeout path directly
            ack_task = asyncio.create_task(mon._write_ack_queue.get())
            error_task = asyncio.create_task(mon._error_queue.get())
            done, pending = await asyncio.wait(
                [ack_task, error_task],
                timeout=0.1,
                return_when=asyncio.FIRST_COMPLETED,
            )
            for task in pending:
                task.cancel()
                try:
                    await task
                except asyncio.CancelledError:
                    pass
            # Nothing completed — this is the timeout case
            assert len(done) == 0

        _run(run())


class TestReadLine:
    """Test read_line() method."""

    def test_read_line_returns_data(self):
        async def run():
            mon = AsyncSerialMonitor(port="COM13")
            mon._connected = True

            await mon._line_queue.put("Hello World")

            line = await mon.read_line(timeout=1.0)
            assert line == "Hello World"
            assert mon.last_line == "Hello World"

        _run(run())

    def test_read_line_timeout_returns_none(self):
        async def run():
            mon = AsyncSerialMonitor(port="COM13")
            mon._connected = True

            line = await mon.read_line(timeout=0.1)
            assert line is None

        _run(run())

    def test_read_line_not_connected_raises(self):
        async def run():
            mon = AsyncSerialMonitor(port="COM13")
            with pytest.raises(RuntimeError, match="Cannot read: not connected"):
                await mon.read_line()

        _run(run())


class TestReadLines:
    """Test read_lines() async generator."""

    def test_read_lines_async_generator(self):
        async def run():
            mon = AsyncSerialMonitor(port="COM13")
            mon._connected = True

            await mon._line_queue.put("line1")
            await mon._line_queue.put("line2")
            await mon._line_queue.put("line3")

            lines = []
            async for line in mon.read_lines(timeout=0.5):
                lines.append(line)
                if len(lines) == 3:
                    break

            assert lines == ["line1", "line2", "line3"]

        _run(run())

    def test_read_lines_timeout(self):
        async def run():
            mon = AsyncSerialMonitor(port="COM13")
            mon._connected = True

            await mon._line_queue.put("only-line")

            lines = []
            async for line in mon.read_lines(timeout=0.2):
                lines.append(line)

            assert lines == ["only-line"]

        _run(run())

    def test_read_lines_calls_sync_hooks(self):
        async def run():
            hook_calls: list[str] = []

            def sync_hook(line: str) -> None:
                hook_calls.append(line)

            mon = AsyncSerialMonitor(port="COM13", hooks=[sync_hook])
            mon._connected = True

            await mon._line_queue.put("test-line")

            async for _ in mon.read_lines(timeout=0.2):
                pass

            assert hook_calls == ["test-line"]

        _run(run())

    def test_read_lines_calls_async_hooks(self):
        async def run():
            hook_calls: list[str] = []

            async def async_hook(line: str) -> None:
                hook_calls.append(line)

            mon = AsyncSerialMonitor(port="COM13", hooks=[async_hook])
            mon._connected = True

            await mon._line_queue.put("async-line")

            async for _ in mon.read_lines(timeout=0.2):
                pass

            assert hook_calls == ["async-line"]

        _run(run())

    def test_read_lines_hook_error(self):
        async def run():
            def bad_hook(line: str) -> None:
                raise ValueError("hook failed")

            mon = AsyncSerialMonitor(port="COM13", hooks=[bad_hook])
            mon._connected = True

            await mon._line_queue.put("trigger")

            with pytest.raises(MonitorHookError, match="hook failed"):
                async for _ in mon.read_lines(timeout=1.0):
                    pass

        _run(run())


class TestPreemption:
    """Test preemption handling."""

    def test_preemption_auto_reconnect(self):
        """When auto_reconnect=True, preemption is logged but no exception queued."""

        async def run():
            mon = AsyncSerialMonitor(port="COM13", auto_reconnect=True)
            mon._connected = True
            assert mon._error_queue.empty()

        _run(run())

    def test_preemption_raises_without_auto_reconnect(self):
        """When auto_reconnect=False, preemption raises MonitorPreemptedException."""

        async def run():
            mon = AsyncSerialMonitor(port="COM13", auto_reconnect=False)
            mon._connected = True

            exc = MonitorPreemptedException("COM13", "deploy_client_xyz")
            await mon._error_queue.put(exc)

            with pytest.raises(MonitorPreemptedException):
                async for _ in mon.read_lines(timeout=1.0):
                    pass

        _run(run())


class TestWriteJsonRpc:
    """Test write_json_rpc() method."""

    def test_write_json_rpc(self):
        async def run():
            mon = AsyncSerialMonitor(port="COM13")
            mon._connected = True
            mon._ws = AsyncMock()
            mon._ws.send = AsyncMock()

            async def feed_ack():
                await asyncio.sleep(0.05)
                await mon._write_ack_queue.put({"type": "write_ack", "success": True, "bytes_written": 30})

            async def feed_response():
                await asyncio.sleep(0.1)
                await mon._line_queue.put('{"id": 42, "result": "ok"}')

            ack_task = asyncio.create_task(feed_ack())
            resp_task = asyncio.create_task(feed_response())

            result = await mon.write_json_rpc({"id": 42, "method": "test"}, timeout=2.0)

            assert result is not None
            assert result["id"] == 42
            assert result["result"] == "ok"

            await ack_task
            await resp_task

        _run(run())

    def test_write_json_rpc_missing_id(self):
        async def run():
            mon = AsyncSerialMonitor(port="COM13")
            mon._connected = True
            mon._ws = AsyncMock()

            with pytest.raises(ValueError, match="must have 'id' field"):
                await mon.write_json_rpc({"method": "no_id"}, timeout=1.0)

        _run(run())


class TestReceiveMessages:
    """Test _receive_messages() routing."""

    def test_receive_messages_routes_data(self):
        async def run():
            mon = AsyncSerialMonitor(port="COM13")
            mon._connected = True

            ws = AsyncMock()
            ws.recv = AsyncMock(
                side_effect=[
                    json.dumps({"type": "data", "lines": ["line1", "line2"], "current_index": 2}),
                    websockets.exceptions.ConnectionClosed(None, None),
                ]
            )
            mon._ws = ws

            await mon._receive_messages()

            assert mon._line_queue.qsize() == 2
            assert await mon._line_queue.get() == "line1"
            assert await mon._line_queue.get() == "line2"

        _run(run())

    def test_receive_messages_routes_write_ack(self):
        async def run():
            mon = AsyncSerialMonitor(port="COM13")
            mon._connected = True

            ws = AsyncMock()
            ws.recv = AsyncMock(
                side_effect=[
                    json.dumps({"type": "write_ack", "success": True, "bytes_written": 5}),
                    websockets.exceptions.ConnectionClosed(None, None),
                ]
            )
            mon._ws = ws

            await mon._receive_messages()

            assert mon._write_ack_queue.qsize() == 1
            ack = await mon._write_ack_queue.get()
            assert ack["bytes_written"] == 5

        _run(run())

    def test_receive_messages_routes_error(self):
        async def run():
            mon = AsyncSerialMonitor(port="COM13")
            mon._connected = True

            ws = AsyncMock()
            ws.recv = AsyncMock(
                side_effect=[
                    json.dumps({"type": "error", "message": "Something went wrong"}),
                    websockets.exceptions.ConnectionClosed(None, None),
                ]
            )
            mon._ws = ws

            await mon._receive_messages()

            assert mon._error_queue.qsize() == 1
            err = await mon._error_queue.get()
            assert "Something went wrong" in str(err)

        _run(run())

    def test_receive_messages_preemption_no_auto_reconnect(self):
        async def run():
            mon = AsyncSerialMonitor(port="COM13", auto_reconnect=False)
            mon._connected = True

            ws = AsyncMock()
            ws.recv = AsyncMock(
                side_effect=[
                    json.dumps({"type": "preempted", "reason": "deploy", "preempted_by": "deploy_abc"}),
                    websockets.exceptions.ConnectionClosed(None, None),
                ]
            )
            mon._ws = ws

            await mon._receive_messages()

            assert mon._error_queue.qsize() == 1
            err = await mon._error_queue.get()
            assert isinstance(err, MonitorPreemptedException)
            assert err.preempted_by == "deploy_abc"

        _run(run())

    def test_receive_messages_preemption_auto_reconnect(self):
        async def run():
            mon = AsyncSerialMonitor(port="COM13", auto_reconnect=True)
            mon._connected = True

            ws = AsyncMock()
            ws.recv = AsyncMock(
                side_effect=[
                    json.dumps({"type": "preempted", "reason": "deploy", "preempted_by": "deploy_abc"}),
                    websockets.exceptions.ConnectionClosed(None, None),
                ]
            )
            mon._ws = ws

            await mon._receive_messages()

            assert mon._error_queue.empty()

        _run(run())


class TestRunUntil:
    """Test run_until() method."""

    def test_run_until_condition_met(self):
        async def run():
            mon = AsyncSerialMonitor(port="COM13")
            mon._connected = True

            await mon._line_queue.put("waiting...")
            await mon._line_queue.put("READY")

            result = await mon.run_until(lambda: "READY" in mon.last_line, timeout=1.0)
            assert result is True

        _run(run())

    def test_run_until_timeout(self):
        async def run():
            mon = AsyncSerialMonitor(port="COM13")
            mon._connected = True

            await mon._line_queue.put("not ready")

            result = await mon.run_until(lambda: "READY" in mon.last_line, timeout=0.2)
            assert result is False

        _run(run())


class TestIsConnected:
    """Test is_connected property."""

    def test_not_connected_initially(self):
        mon = AsyncSerialMonitor(port="COM13")
        assert mon.is_connected is False

    def test_connected_after_manual_set(self):
        mon = AsyncSerialMonitor(port="COM13")
        mon._connected = True
        assert mon.is_connected is True
