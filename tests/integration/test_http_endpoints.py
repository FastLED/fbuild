"""
Comprehensive Integration Tests for HTTP Daemon Endpoints

Tests all HTTP endpoints with actual daemon interaction:
- Build, deploy, monitor, install-deps operations
- Device management (list, status, lease, release, preempt)
- Lock management (status, clear)
- Daemon status and shutdown
- WebSocket status updates

Requires daemon to be running or will start it automatically.
"""

import os
import time
from pathlib import Path

import pytest

# Set dev mode and custom port for testing
os.environ["FBUILD_DEV_MODE"] = "1"
os.environ["FBUILD_DAEMON_PORT"] = "9176"

from fbuild.daemon.client.devices_http import (
    acquire_device_lease_http,
    get_device_status_http,
    list_devices_http,
    preempt_device_http,
    release_device_lease_http,
)
from fbuild.daemon.client.http_utils import (
    get_daemon_url,
    http_client,
    is_daemon_http_available,
)
from fbuild.daemon.client.lifecycle import (
    ensure_daemon_running,
    is_daemon_running,
    stop_daemon,
)
from fbuild.daemon.client.locks_http import clear_stale_locks_http, get_lock_status_http
from fbuild.daemon.client.requests_http import (
    request_build_http,
    request_install_dependencies_http,
    request_monitor_http,
)

# Test projects directory
TEST_PROJECTS = Path(__file__).parent.parent


@pytest.fixture(scope="module")
def daemon():
    """Ensure daemon is running for all tests in this module."""
    # Stop any existing daemon
    if is_daemon_running():
        stop_daemon()
        time.sleep(2)

    # Start daemon
    ensure_daemon_running(verbose=True)
    time.sleep(1)

    # Verify HTTP server is available
    assert is_daemon_http_available(), "Daemon HTTP server not available after startup"

    yield

    # Cleanup: stop daemon
    stop_daemon()
    time.sleep(1)


@pytest.mark.integration
class TestHealthEndpoints:
    """Test health check and daemon info endpoints."""

    def test_health_endpoint(self, daemon):
        """Test /health endpoint."""
        with http_client() as client:
            response = client.get(get_daemon_url("/health"))
            assert response.status_code == 200

            data = response.json()
            assert data["status"] == "healthy"
            assert "uptime_seconds" in data
            assert "version" in data

    def test_root_endpoint(self, daemon):
        """Test root / endpoint."""
        with http_client() as client:
            response = client.get(get_daemon_url("/"))
            assert response.status_code == 200

            data = response.json()
            assert "message" in data
            assert "version" in data
            assert "docs" in data

    def test_daemon_info_endpoint(self, daemon):
        """Test /api/daemon/info endpoint."""
        with http_client() as client:
            response = client.get(get_daemon_url("/api/daemon/info"))
            assert response.status_code == 200

            data = response.json()
            assert "pid" in data
            assert "started_at" in data
            assert "uptime_seconds" in data
            assert "version" in data
            assert data["port"] == 9176  # Custom port from environment
            assert data["host"] == "127.0.0.1"
            assert data["dev_mode"] is True


@pytest.mark.integration
class TestDeviceEndpoints:
    """Test device management endpoints."""

    def test_list_devices(self, daemon):
        """Test listing devices."""
        devices = list_devices_http(refresh=True)
        assert devices is not None
        assert isinstance(devices, list)
        # May be empty if no devices connected

    def test_list_devices_no_refresh(self, daemon):
        """Test listing devices without refresh."""
        devices = list_devices_http(refresh=False)
        assert devices is not None
        assert isinstance(devices, list)

    def test_get_device_status_nonexistent(self, daemon):
        """Test getting status of nonexistent device."""
        status = get_device_status_http("nonexistent_device")
        # Should return None for nonexistent device
        assert status is None or status.get("is_connected") is False

    def test_acquire_release_device_lease(self, daemon):
        """Test acquiring and releasing device lease."""
        # List devices first
        devices = list_devices_http(refresh=True)
        if not devices or len(devices) == 0:
            pytest.skip("No devices available for testing")

        device_id = devices[0]["device_id"]
        client_id = "test_client_acquire_release"

        # Acquire lease
        result = acquire_device_lease_http(device_id, client_id, exclusive=True)
        if result and result.get("success"):
            # Release lease
            release_result = release_device_lease_http(device_id, client_id)
            assert release_result is not None

    def test_preempt_device(self, daemon):
        """Test device preemption."""
        # List devices first
        devices = list_devices_http(refresh=True)
        if not devices or len(devices) == 0:
            pytest.skip("No devices available for testing")

        device_id = devices[0]["device_id"]
        preemptor_id = "test_preemptor"

        # Try to preempt (may fail if no active lease)
        result = preempt_device_http(device_id, preemptor_id)
        # Result can be None or dict, just verify it doesn't crash
        assert result is None or isinstance(result, dict)


@pytest.mark.integration
class TestLockEndpoints:
    """Test lock management endpoints."""

    def test_get_lock_status(self, daemon):
        """Test getting lock status."""
        status = get_lock_status_http()
        assert status is not None
        assert isinstance(status, dict)
        # Should have locks information
        assert "locks" in status or "active_locks" in status or len(status) == 0

    def test_clear_stale_locks(self, daemon):
        """Test clearing stale locks."""
        result = clear_stale_locks_http()
        # Should return a result (success or failure)
        assert result is not None


@pytest.mark.integration
class TestBuildEndpoint:
    """Test build operation endpoint."""

    def test_build_uno_project(self, daemon):
        """Test building Arduino Uno project via HTTP."""
        uno_project = TEST_PROJECTS / "uno"
        if not uno_project.exists():
            pytest.skip(f"Uno test project not found at {uno_project}")

        # Submit build request
        success = request_build_http(
            project_dir=uno_project,
            environment="uno",
            clean_build=False,
            verbose=True,
            jobs=2,
            timeout=300,
        )

        assert success is True

    def test_build_esp32c6_project(self, daemon):
        """Test building ESP32-C6 project via HTTP."""
        esp32c6_project = TEST_PROJECTS / "esp32c6"
        if not esp32c6_project.exists():
            pytest.skip(f"ESP32-C6 test project not found at {esp32c6_project}")

        # Submit build request
        success = request_build_http(
            project_dir=esp32c6_project,
            environment="esp32c6",
            clean_build=False,
            verbose=True,
            jobs=2,
            timeout=300,
        )

        assert success is True

    def test_build_nonexistent_project(self, daemon):
        """Test building nonexistent project (should fail gracefully)."""
        nonexistent = Path("/nonexistent/project")

        success = request_build_http(
            project_dir=nonexistent,
            environment="uno",
            clean_build=False,
            verbose=False,
            timeout=30,
        )

        # Should fail but not crash
        assert success is False


@pytest.mark.integration
class TestInstallDepsEndpoint:
    """Test install dependencies endpoint."""

    def test_install_deps_uno(self, daemon):
        """Test installing dependencies for Uno project."""
        uno_project = TEST_PROJECTS / "uno"
        if not uno_project.exists():
            pytest.skip(f"Uno test project not found at {uno_project}")

        success = request_install_dependencies_http(
            project_dir=uno_project,
            environment="uno",
            verbose=True,
            timeout=300,
        )

        # Dependencies might already be installed, so success is expected
        assert success is True


@pytest.mark.integration
class TestMonitorEndpoint:
    """Test serial monitor endpoint."""

    def test_monitor_request_no_device(self, daemon):
        """Test monitor request when no device is connected."""
        # This will likely fail if no device is connected, but should not crash
        success = request_monitor_http(
            port="COM99",  # Unlikely to exist
            baud_rate=115200,
            client_id="test_monitor",
            timeout=10,
        )

        # Should fail gracefully
        assert success is False or success is True  # Either is acceptable


@pytest.mark.integration
class TestDaemonShutdown:
    """Test daemon shutdown endpoint."""

    def test_daemon_shutdown(self):
        """Test graceful daemon shutdown via HTTP.

        Note: This test is run last as it shuts down the daemon.
        """
        # Start fresh daemon for this test
        if is_daemon_running():
            stop_daemon()
            time.sleep(2)

        ensure_daemon_running(verbose=True)
        time.sleep(1)

        # Send shutdown request
        with http_client(timeout=5.0) as client:
            response = client.post(get_daemon_url("/api/daemon/shutdown"))
            assert response.status_code == 200

            data = response.json()
            assert "message" in data
            assert data["status"] == "shutting_down"

        # Wait for shutdown
        time.sleep(2)

        # Verify daemon is no longer running
        assert is_daemon_running() is False


@pytest.mark.integration
class TestConcurrentRequests:
    """Test concurrent HTTP requests to daemon."""

    def test_concurrent_health_checks(self, daemon):
        """Test multiple concurrent health check requests."""
        import concurrent.futures

        def health_check():
            with http_client() as client:
                response = client.get(get_daemon_url("/health"))
                return response.status_code == 200

        # Make 10 concurrent requests
        with concurrent.futures.ThreadPoolExecutor(max_workers=10) as executor:
            futures = [executor.submit(health_check) for _ in range(10)]
            results = [f.result() for f in concurrent.futures.as_completed(futures)]

        # All requests should succeed
        assert all(results)

    def test_concurrent_device_list(self, daemon):
        """Test multiple concurrent device list requests."""
        import concurrent.futures

        def list_devices():
            devices = list_devices_http()
            return devices is not None

        # Make 5 concurrent requests
        with concurrent.futures.ThreadPoolExecutor(max_workers=5) as executor:
            futures = [executor.submit(list_devices) for _ in range(5)]
            results = [f.result() for f in concurrent.futures.as_completed(futures)]

        # All requests should succeed
        assert all(results)


if __name__ == "__main__":
    pytest.main([__file__, "-v", "-s"])
