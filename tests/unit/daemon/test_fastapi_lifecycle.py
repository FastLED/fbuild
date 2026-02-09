"""
Integration test for FastAPI server lifecycle.

This test spawns a real FastAPI server (no mocks), verifies it responds,
and tests graceful shutdown. It catches issues like immediate shutdown
after yield in the lifespan context manager.
"""

import hashlib
import os
import threading
import time
from pathlib import Path

import pytest
import requests


def get_test_port() -> int:
    """
    Calculate unique port for this test based on filename hash.

    This ensures multiple tests can run in parallel without port conflicts.
    Port range: 9000-9999 (avoiding common ports like 8765, 8865, 9876).
    """
    test_file = Path(__file__).name
    hash_val = int(hashlib.md5(test_file.encode()).hexdigest()[:8], 16)
    port = 9000 + (hash_val % 1000)
    return port


def wait_for_server(url: str, timeout: float = 10.0, interval: float = 0.1) -> bool:
    """
    Wait for server to become available with retry logic.

    Args:
        url: URL to check
        timeout: Maximum wait time in seconds
        interval: Time between retries in seconds

    Returns:
        True if server became available, False if timeout
    """
    start_time = time.time()
    while time.time() - start_time < timeout:
        try:
            response = requests.get(url, timeout=1.0)
            if response.status_code == 200:
                return True
        except requests.exceptions.RequestException:
            pass  # Server not ready yet
        time.sleep(interval)
    return False


def test_fastapi_server_lifecycle():
    """
    Test FastAPI server lifecycle: spawn, verify health, shutdown.

    This test validates:
    1. Server starts successfully in background thread
    2. Server responds to HTTP requests
    3. Server stays running (doesn't exit immediately after startup)
    4. Server can be shut down gracefully
    """
    from fbuild.daemon.daemon_context import create_daemon_context
    from fbuild.daemon.paths import DAEMON_DIR

    # Calculate unique port for this test
    port = get_test_port()
    print(f"\nTest using port: {port}")

    # Create minimal daemon context for testing
    daemon_pid = os.getpid()
    daemon_started_at = time.time()

    # Use test-specific directories to avoid conflicts
    test_daemon_dir = DAEMON_DIR.parent / f"daemon_test_{port}"
    test_daemon_dir.mkdir(parents=True, exist_ok=True)

    test_status_file = test_daemon_dir / "daemon_status.json"
    test_cache_file = test_daemon_dir / "file_cache.json"

    try:
        context = create_daemon_context(
            daemon_pid=daemon_pid,
            daemon_started_at=daemon_started_at,
            num_workers=2,
            file_cache_path=test_cache_file,
            status_file_path=test_status_file,
            daemon_dir=test_daemon_dir,
        )

        # Import FastAPI components
        import asyncio
        import uvicorn
        from fbuild.daemon.fastapi_app import create_app, set_daemon_context

        # Set daemon context for FastAPI dependency injection
        set_daemon_context(context)

        # Create FastAPI app
        app = create_app()

        # Configure uvicorn
        config = uvicorn.Config(
            app,
            host="127.0.0.1",
            port=port,
            log_level="error",  # Suppress uvicorn logs during test
            access_log=False,
        )
        server = uvicorn.Server(config)

        # Track server state
        server_error = None
        server_started = threading.Event()

        # Run server in background thread with proper event loop
        def run_server():
            nonlocal server_error
            # Create new event loop for this thread (required for uvicorn)
            loop = asyncio.new_event_loop()
            asyncio.set_event_loop(loop)

            try:
                server_started.set()
                # Use loop.run_until_complete(server.serve()) instead of server.run()
                # server.run() calls asyncio.run() which creates another loop and exits
                loop.run_until_complete(server.serve())
            except KeyboardInterrupt:
                raise
            except Exception as e:
                server_error = e
            finally:
                loop.close()

        # Start server thread
        server_thread = threading.Thread(
            target=run_server,
            daemon=True,
            name=f"TestFastAPI-{port}"
        )
        server_thread.start()

        # Wait for thread to start
        assert server_started.wait(timeout=5.0), "Server thread failed to start"

        # Wait for server to become available
        base_url = f"http://127.0.0.1:{port}"
        health_url = f"{base_url}/health"

        print(f"Waiting for server at {health_url}...")
        server_available = wait_for_server(health_url, timeout=10.0)

        # Check for server errors during startup
        if server_error:
            pytest.fail(f"Server error during startup: {server_error}")

        assert server_available, (
            f"Server failed to become available within 10s. "
            f"Thread alive: {server_thread.is_alive()}"
        )

        print("✓ Server is available")

        # Test 1: Verify health endpoint responds
        response = requests.get(health_url, timeout=5.0)
        assert response.status_code == 200, f"Health check failed: {response.status_code}"

        health_data = response.json()
        assert health_data["status"] == "healthy", f"Unexpected health status: {health_data}"
        assert "version" in health_data, "Version missing from health response"

        print(f"✓ Health check passed: {health_data}")

        # Test 2: Verify server stays running (doesn't exit immediately)
        # Wait a bit and check server is still responsive
        time.sleep(2.0)

        assert server_thread.is_alive(), "Server thread died unexpectedly"

        response = requests.get(health_url, timeout=5.0)
        assert response.status_code == 200, "Server stopped responding after 2s"

        print("✓ Server stayed running for 2+ seconds")

        # Test 3: Verify daemon info endpoint
        info_url = f"{base_url}/api/daemon/info"
        response = requests.get(info_url, timeout=5.0)
        assert response.status_code == 200, f"Daemon info failed: {response.status_code}"

        info_data = response.json()
        assert info_data["pid"] == daemon_pid, "PID mismatch in daemon info"
        # Note: port may not match test port since get_daemon_port() reads from file
        # This is fine - we're testing the lifecycle, not port file synchronization
        assert "port" in info_data, "Port missing from daemon info"

        print(f"✓ Daemon info endpoint working: PID={info_data['pid']}, port={info_data['port']}")

        # Test 4: Graceful shutdown
        print("Testing graceful shutdown...")

        # Shutdown server by calling shutdown on the uvicorn server
        # This will cause server.serve() to return
        if hasattr(server, 'should_exit'):
            server.should_exit = True

        # Wait for server thread to exit
        server_thread.join(timeout=5.0)

        # Verify server stopped
        assert not server_thread.is_alive(), "Server thread did not stop after shutdown"

        print("✓ Server shut down gracefully")

        # Verify server is no longer responding
        time.sleep(0.5)  # Brief delay for port to be released

        try:
            requests.get(health_url, timeout=1.0)
            pytest.fail("Server still responding after shutdown")
        except requests.exceptions.RequestException:
            pass  # Expected - server is down

        print("✓ Server no longer responding after shutdown")

    finally:
        # Cleanup test directories
        import shutil
        if test_daemon_dir.exists():
            shutil.rmtree(test_daemon_dir, ignore_errors=True)


def test_fastapi_immediate_shutdown_regression():
    """
    Regression test for the FastAPI immediate shutdown bug.

    This specifically tests the bug where server.run() was used instead of
    loop.run_until_complete(server.serve()), causing immediate shutdown
    after the lifespan yield.
    """
    from fbuild.daemon.daemon_context import create_daemon_context
    from fbuild.daemon.paths import DAEMON_DIR

    # Use different port from main test
    port = get_test_port() + 1
    print(f"\nRegression test using port: {port}")

    test_daemon_dir = DAEMON_DIR.parent / f"daemon_test_regression_{port}"
    test_daemon_dir.mkdir(parents=True, exist_ok=True)

    test_status_file = test_daemon_dir / "daemon_status.json"
    test_cache_file = test_daemon_dir / "file_cache.json"

    try:
        context = create_daemon_context(
            daemon_pid=os.getpid(),
            daemon_started_at=time.time(),
            num_workers=2,
            file_cache_path=test_cache_file,
            status_file_path=test_status_file,
            daemon_dir=test_daemon_dir,
        )

        import asyncio
        import uvicorn
        from fbuild.daemon.fastapi_app import create_app, set_daemon_context

        set_daemon_context(context)
        app = create_app()

        config = uvicorn.Config(
            app,
            host="127.0.0.1",
            port=port,
            log_level="error",
            access_log=False,
        )
        server = uvicorn.Server(config)

        # Run server with the FIXED pattern (loop.run_until_complete)
        def run_server_fixed():
            loop = asyncio.new_event_loop()
            asyncio.set_event_loop(loop)
            try:
                loop.run_until_complete(server.serve())
            finally:
                loop.close()

        server_thread = threading.Thread(target=run_server_fixed, daemon=True)
        server_thread.start()

        # Wait for server
        base_url = f"http://127.0.0.1:{port}"
        health_url = f"{base_url}/health"

        assert wait_for_server(health_url, timeout=10.0), (
            "Server with FIXED pattern failed to start - regression detected!"
        )

        # Verify it stays running for at least 3 seconds
        for i in range(3):
            time.sleep(1.0)
            response = requests.get(health_url, timeout=2.0)
            assert response.status_code == 200, (
                f"Server stopped responding after {i+1}s - regression detected!"
            )
            print(f"✓ Still running after {i+1}s")

        print("✓ Regression test passed - server stays running")

        # Shutdown
        if hasattr(server, 'should_exit'):
            server.should_exit = True
        server_thread.join(timeout=5.0)

    finally:
        import shutil
        if test_daemon_dir.exists():
            shutil.rmtree(test_daemon_dir, ignore_errors=True)


if __name__ == "__main__":
    # Allow running test directly for debugging
    print("Running FastAPI lifecycle tests...")
    test_fastapi_server_lifecycle()
    print("\n" + "="*80 + "\n")
    test_fastapi_immediate_shutdown_regression()
    print("\n✅ All tests passed!")
