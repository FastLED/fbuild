"""
Tests for interruptible HTTP client.

This module tests that HTTP requests can be properly interrupted by CTRL-C,
fixing the Windows blocking I/O issue where KeyboardInterrupt doesn't work.
"""

import threading
import time
from http.server import BaseHTTPRequestHandler, HTTPServer
from typing import Any

import pytest

from fbuild.daemon.client.interruptible_http import (
    InterruptibleHTTPError,
    interruptible_get,
    interruptible_post,
)


class SlowHTTPRequestHandler(BaseHTTPRequestHandler):
    """Test HTTP server that responds slowly or hangs."""

    # Class variable to control server behavior
    delay_seconds: float = 0.0
    hang_forever: bool = False

    def do_GET(self) -> None:
        """Handle GET request with configurable delay."""
        if self.hang_forever:
            # Hang forever (simulates unresponsive daemon)
            while True:
                time.sleep(1)

        # Delay before responding
        time.sleep(self.delay_seconds)

        # Send successful response
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(b'{"status": "ok"}')

    def do_POST(self) -> None:
        """Handle POST request with configurable delay."""
        if self.hang_forever:
            # Hang forever (simulates unresponsive daemon)
            while True:
                time.sleep(1)

        # Delay before responding
        time.sleep(self.delay_seconds)

        # Send successful response
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(b'{"success": true}')

    def log_message(self, format: str, *args: Any) -> None:
        """Suppress request logging."""
        pass


@pytest.fixture
def http_server() -> Any:
    """Start a test HTTP server on a random port."""
    # Find available port
    import socket

    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]

    # Reset class variables
    SlowHTTPRequestHandler.delay_seconds = 0.0
    SlowHTTPRequestHandler.hang_forever = False

    # Start server in background thread
    server = HTTPServer(("127.0.0.1", port), SlowHTTPRequestHandler)
    server_thread = threading.Thread(target=server.serve_forever, daemon=True)
    server_thread.start()

    yield server, port

    # Cleanup
    server.shutdown()
    server_thread.join(timeout=2.0)


def test_interruptible_post_success(http_server: Any) -> None:
    """Test that interruptible_post works for successful requests."""
    server, port = http_server

    # Make a successful POST request
    response = interruptible_post(
        url=f"http://127.0.0.1:{port}/test",
        json={"data": "test"},
        timeout=5.0,
    )

    assert response.status_code == 200
    assert response.json() == {"success": True}


def test_interruptible_get_success(http_server: Any) -> None:
    """Test that interruptible_get works for successful requests."""
    server, port = http_server

    # Make a successful GET request
    response = interruptible_get(
        url=f"http://127.0.0.1:{port}/test",
        timeout=5.0,
    )

    assert response.status_code == 200
    assert response.json() == {"status": "ok"}


def test_interruptible_post_with_keyboard_interrupt(http_server: Any) -> None:
    """Test that interruptible_post can be interrupted by KeyboardInterrupt."""
    server, port = http_server

    # Configure server to delay for a long time
    SlowHTTPRequestHandler.delay_seconds = 10.0

    # Schedule a KeyboardInterrupt to be raised after 0.5 seconds
    def send_interrupt() -> None:
        """Send KeyboardInterrupt after short delay."""
        time.sleep(0.5)
        # Simulate CTRL-C by raising KeyboardInterrupt in the main thread
        import _thread

        _thread.interrupt_main()

    interrupt_thread = threading.Thread(target=send_interrupt, daemon=True)
    interrupt_thread.start()

    # Make request that should be interrupted
    start_time = time.time()
    with pytest.raises(KeyboardInterrupt):
        interruptible_post(
            url=f"http://127.0.0.1:{port}/test",
            json={"data": "test"},
            timeout=15.0,
        )

    elapsed = time.time() - start_time

    # Should be interrupted quickly (within 1 second), not after the full delay
    assert elapsed < 2.0, f"Request took {elapsed:.1f}s to interrupt (should be < 2s)"


def test_interruptible_post_timeout(http_server: Any) -> None:
    """Test that interruptible_post times out properly."""
    server, port = http_server

    # Configure server to delay longer than timeout
    SlowHTTPRequestHandler.delay_seconds = 5.0

    # Make request with short timeout
    start_time = time.time()
    with pytest.raises(InterruptibleHTTPError) as exc_info:
        interruptible_post(
            url=f"http://127.0.0.1:{port}/test",
            json={"data": "test"},
            timeout=1.0,
        )

    elapsed = time.time() - start_time

    # Should timeout after approximately 1 second
    assert 0.8 < elapsed < 3.0, f"Timeout took {elapsed:.1f}s (expected ~1s)"
    # Check for "timed out" (what httpx.TimeoutException produces)
    assert "timed out" in str(exc_info.value).lower() or "timeout" in str(exc_info.value).lower()


def test_interruptible_post_connection_error() -> None:
    """Test that interruptible_post handles connection errors."""
    # Try to connect to a port that's not listening
    with pytest.raises(InterruptibleHTTPError) as exc_info:
        interruptible_post(
            url="http://127.0.0.1:9999/test",
            json={"data": "test"},
            timeout=2.0,
            connect_timeout=0.5,
        )

    error_msg = str(exc_info.value).lower()
    # Connection to non-listening port may raise connection error or timeout
    assert any(keyword in error_msg for keyword in ["connect", "connection", "timed out", "timeout", "refused"]), f"Expected connection-related error, got: {error_msg}"


@pytest.mark.skip(reason="This test hangs indefinitely - only run manually to verify interrupt behavior")
def test_interruptible_post_hang_scenario(http_server: Any) -> None:
    """Test that interruptible_post can interrupt a hung request.

    This test is skipped by default because it requires manual CTRL-C.
    To test manually:
    1. Comment out the @pytest.mark.skip decorator
    2. Run: pytest -v tests/unit/daemon/test_interruptible_http.py::test_interruptible_post_hang_scenario
    3. Press CTRL-C after a few seconds
    4. Verify that the test stops immediately (not after timeout)
    """
    server, port = http_server

    # Configure server to hang forever
    SlowHTTPRequestHandler.hang_forever = True

    print("\nServer is hanging - press CTRL-C to test interrupt behavior...")

    # Make request that will hang
    with pytest.raises(KeyboardInterrupt):
        interruptible_post(
            url=f"http://127.0.0.1:{port}/test",
            json={"data": "test"},
            timeout=300.0,  # 5 minute timeout
        )


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
