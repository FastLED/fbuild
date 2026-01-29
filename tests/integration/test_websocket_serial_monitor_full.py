"""
Comprehensive WebSocket Serial Monitor Tests

Tests the WebSocket-based serial monitor implementation:
- Attach/detach operations
- Data streaming
- Write operations
- Preemption handling
- Reconnection handling
- Concurrent client handling
- Error scenarios

Requires daemon running with WebSocket support.
"""

import asyncio
import json
import os
import time

import pytest
import websockets

# Set test environment
os.environ["FBUILD_DEV_MODE"] = "1"
os.environ["FBUILD_DAEMON_PORT"] = "9176"

from fbuild.daemon.client.devices_http import list_devices_http
from fbuild.daemon.client.http_utils import get_daemon_base_url
from fbuild.daemon.client.lifecycle import (
    ensure_daemon_running,
    is_daemon_running,
    stop_daemon,
)


@pytest.fixture(scope="module")
def daemon():
    """Ensure daemon is running with WebSocket support."""
    # Stop any existing daemon
    if is_daemon_running():
        stop_daemon()
        time.sleep(2)

    # Start daemon
    ensure_daemon_running(verbose=True)
    time.sleep(1)

    yield

    # Cleanup
    stop_daemon()
    time.sleep(1)


@pytest.fixture
def serial_device():
    """Find a serial device for testing."""
    devices = list_devices_http(refresh=True)
    if not devices or len(devices) == 0:
        pytest.skip("No serial devices available for testing")

    # Return first connected device
    for device in devices:
        if device.get("is_connected"):
            return device

    pytest.skip("No connected devices found")


def get_websocket_url(endpoint: str) -> str:
    """Get WebSocket URL for daemon endpoint."""
    base_url = get_daemon_base_url()
    ws_url = base_url.replace("http://", "ws://")
    return f"{ws_url}{endpoint}"


@pytest.mark.integration
class TestWebSocketSerialMonitorBasic:
    """Basic WebSocket serial monitor tests."""

    @pytest.mark.asyncio
    async def test_websocket_connection(self, daemon):
        """Test basic WebSocket connection to serial monitor endpoint."""
        ws_url = get_websocket_url("/ws/serial-monitor")

        async with websockets.connect(ws_url) as websocket:
            # Connection successful
            assert websocket.open

    @pytest.mark.asyncio
    async def test_attach_detach_cycle(self, daemon, serial_device):
        """Test attach and detach cycle for serial monitor."""
        ws_url = get_websocket_url("/ws/serial-monitor")
        port = serial_device.get("port")

        async with websockets.connect(ws_url) as websocket:
            # Send attach request
            attach_msg = {
                "type": "attach",
                "client_id": "test_attach_detach",
                "port": port,
                "baud_rate": 115200,
                "open_if_needed": True,
            }
            await websocket.send(json.dumps(attach_msg))

            # Wait for attached response
            response = await asyncio.wait_for(websocket.recv(), timeout=5.0)
            data = json.loads(response)

            assert data.get("type") == "attached"
            assert data.get("success") is True

            # Send detach request
            detach_msg = {"type": "detach"}
            await websocket.send(json.dumps(detach_msg))

            # Wait a moment for detach to process
            await asyncio.sleep(0.5)

    @pytest.mark.asyncio
    async def test_ping_pong(self, daemon):
        """Test ping-pong heartbeat mechanism."""
        ws_url = get_websocket_url("/ws/serial-monitor")

        async with websockets.connect(ws_url) as websocket:
            # Send ping
            ping_msg = {"type": "ping"}
            await websocket.send(json.dumps(ping_msg))

            # Wait for pong
            response = await asyncio.wait_for(websocket.recv(), timeout=5.0)
            data = json.loads(response)

            assert data.get("type") == "pong"
            assert "timestamp" in data


@pytest.mark.integration
class TestWebSocketSerialMonitorDataStreaming:
    """Test data streaming over WebSocket."""

    @pytest.mark.asyncio
    async def test_receive_serial_data(self, daemon, serial_device):
        """Test receiving serial data via WebSocket."""
        ws_url = get_websocket_url("/ws/serial-monitor")
        port = serial_device.get("port")

        async with websockets.connect(ws_url) as websocket:
            # Attach to device
            attach_msg = {
                "type": "attach",
                "client_id": "test_receive_data",
                "port": port,
                "baud_rate": 115200,
                "open_if_needed": True,
            }
            await websocket.send(json.dumps(attach_msg))

            # Wait for attached response
            response = await asyncio.wait_for(websocket.recv(), timeout=5.0)
            data = json.loads(response)
            assert data.get("success") is True

            # Wait for data messages (with timeout)
            try:
                for _ in range(3):  # Try to receive a few messages
                    msg = await asyncio.wait_for(websocket.recv(), timeout=2.0)
                    msg_data = json.loads(msg)

                    if msg_data.get("type") == "data":
                        # Verify data message format
                        assert "lines" in msg_data or "current_index" in msg_data
                        print(f"Received data: {msg_data}")
                        break
            except asyncio.TimeoutError:
                # No data available (device might not be sending)
                print("No data received (device might be idle)")

            # Detach
            await websocket.send(json.dumps({"type": "detach"}))


@pytest.mark.integration
class TestWebSocketSerialMonitorWrite:
    """Test writing data to serial port via WebSocket."""

    @pytest.mark.asyncio
    async def test_write_to_serial(self, daemon, serial_device):
        """Test writing data to serial port."""
        ws_url = get_websocket_url("/ws/serial-monitor")
        port = serial_device.get("port")

        async with websockets.connect(ws_url) as websocket:
            # Attach
            attach_msg = {
                "type": "attach",
                "client_id": "test_write",
                "port": port,
                "baud_rate": 115200,
                "open_if_needed": True,
            }
            await websocket.send(json.dumps(attach_msg))

            # Wait for attach confirmation
            response = await asyncio.wait_for(websocket.recv(), timeout=5.0)
            assert json.loads(response).get("success") is True

            # Write data (base64 encoded)
            import base64

            test_data = b"Hello, ESP32!\n"
            encoded_data = base64.b64encode(test_data).decode("utf-8")

            write_msg = {"type": "write", "data": encoded_data}
            await websocket.send(json.dumps(write_msg))

            # Wait for write acknowledgment
            try:
                response = await asyncio.wait_for(websocket.recv(), timeout=2.0)
                msg_data = json.loads(response)

                if msg_data.get("type") == "write_ack":
                    assert msg_data.get("success") is True
                    print(f"Write successful: {msg_data.get('bytes_written')} bytes")
            except asyncio.TimeoutError:
                print("No write acknowledgment received (might be async)")

            # Detach
            await websocket.send(json.dumps({"type": "detach"}))


@pytest.mark.integration
class TestWebSocketSerialMonitorPreemption:
    """Test preemption handling in WebSocket serial monitor."""

    @pytest.mark.asyncio
    async def test_deploy_preempts_monitor(self, daemon, serial_device):
        """Test that deploy operation preempts active monitor session."""
        ws_url = get_websocket_url("/ws/serial-monitor")
        port = serial_device.get("port")

        async with websockets.connect(ws_url) as websocket:
            # Attach monitor
            attach_msg = {
                "type": "attach",
                "client_id": "test_preemption",
                "port": port,
                "baud_rate": 115200,
                "open_if_needed": True,
            }
            await websocket.send(json.dumps(attach_msg))

            # Wait for attach confirmation
            response = await asyncio.wait_for(websocket.recv(), timeout=5.0)
            assert json.loads(response).get("success") is True

            # TODO: Simulate deploy operation to trigger preemption
            # This would require actually running a deploy operation
            # For now, just test that the monitor is attached

            # Detach
            await websocket.send(json.dumps({"type": "detach"}))


@pytest.mark.integration
class TestWebSocketSerialMonitorConcurrent:
    """Test concurrent WebSocket connections."""

    @pytest.mark.asyncio
    async def test_multiple_concurrent_connections(self, daemon, serial_device):
        """Test multiple concurrent WebSocket connections."""
        ws_url = get_websocket_url("/ws/serial-monitor")
        port = serial_device.get("port")

        async def client_session(client_id: str):
            """Individual client session."""
            async with websockets.connect(ws_url) as websocket:
                # Attach
                attach_msg = {
                    "type": "attach",
                    "client_id": client_id,
                    "port": port,
                    "baud_rate": 115200,
                    "open_if_needed": False,  # Don't open, just monitor
                }
                await websocket.send(json.dumps(attach_msg))

                # Wait for response
                response = await asyncio.wait_for(websocket.recv(), timeout=5.0)
                data = json.loads(response)

                # May succeed or fail depending on port state
                return data.get("success")

        # Run 3 concurrent clients
        results = await asyncio.gather(
            client_session("client_1"),
            client_session("client_2"),
            client_session("client_3"),
        )

        # At least one should succeed (first one gets exclusive access)
        assert any(results), "No client sessions succeeded"


@pytest.mark.integration
class TestWebSocketSerialMonitorErrors:
    """Test error handling in WebSocket serial monitor."""

    @pytest.mark.asyncio
    async def test_attach_invalid_port(self, daemon):
        """Test attaching to invalid port."""
        ws_url = get_websocket_url("/ws/serial-monitor")

        async with websockets.connect(ws_url) as websocket:
            # Try to attach to nonexistent port
            attach_msg = {
                "type": "attach",
                "client_id": "test_invalid_port",
                "port": "COM999",  # Unlikely to exist
                "baud_rate": 115200,
                "open_if_needed": True,
            }
            await websocket.send(json.dumps(attach_msg))

            # Wait for error response
            response = await asyncio.wait_for(websocket.recv(), timeout=5.0)
            data = json.loads(response)

            # Should fail
            assert data.get("type") in ["attached", "error"]
            if data.get("type") == "attached":
                assert data.get("success") is False

    @pytest.mark.asyncio
    async def test_invalid_message_type(self, daemon):
        """Test sending invalid message type."""
        ws_url = get_websocket_url("/ws/serial-monitor")

        async with websockets.connect(ws_url) as websocket:
            # Send invalid message
            invalid_msg = {"type": "invalid_type"}
            await websocket.send(json.dumps(invalid_msg))

            # Wait for error response (or timeout)
            try:
                response = await asyncio.wait_for(websocket.recv(), timeout=2.0)
                data = json.loads(response)

                # Should receive error
                assert data.get("type") == "error"
            except asyncio.TimeoutError:
                # Server might ignore invalid messages
                pass

    @pytest.mark.asyncio
    async def test_malformed_json(self, daemon):
        """Test sending malformed JSON."""
        ws_url = get_websocket_url("/ws/serial-monitor")

        async with websockets.connect(ws_url) as websocket:
            # Send malformed JSON
            await websocket.send("{invalid json")

            # Connection might be closed or error returned
            try:
                response = await asyncio.wait_for(websocket.recv(), timeout=2.0)
                data = json.loads(response)
                assert data.get("type") == "error"
            except (asyncio.TimeoutError, websockets.exceptions.ConnectionClosed):
                # Expected for malformed JSON
                pass


@pytest.mark.integration
class TestWebSocketSerialMonitorReconnection:
    """Test reconnection scenarios."""

    @pytest.mark.asyncio
    async def test_reconnect_after_disconnect(self, daemon, serial_device):
        """Test reconnecting after disconnect."""
        ws_url = get_websocket_url("/ws/serial-monitor")
        port = serial_device.get("port")

        # First connection
        async with websockets.connect(ws_url) as websocket:
            attach_msg = {
                "type": "attach",
                "client_id": "test_reconnect",
                "port": port,
                "baud_rate": 115200,
                "open_if_needed": True,
            }
            await websocket.send(json.dumps(attach_msg))

            response = await asyncio.wait_for(websocket.recv(), timeout=5.0)
            assert json.loads(response).get("success") is True

        # Wait a moment
        await asyncio.sleep(1)

        # Second connection (reconnect)
        async with websockets.connect(ws_url) as websocket:
            attach_msg = {
                "type": "attach",
                "client_id": "test_reconnect",  # Same client ID
                "port": port,
                "baud_rate": 115200,
                "open_if_needed": True,
            }
            await websocket.send(json.dumps(attach_msg))

            response = await asyncio.wait_for(websocket.recv(), timeout=5.0)
            data = json.loads(response)

            # Should succeed or indicate reconnection
            assert data.get("type") in ["attached", "reconnected"]


if __name__ == "__main__":
    pytest.main([__file__, "-v", "-s"])
