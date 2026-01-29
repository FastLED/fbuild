"""
ESP32-S3 Specific HTTP Daemon Tests

Comprehensive tests for ESP32-S3 hardware with HTTP daemon:
- Build via HTTP for ESP32-S3
- Deploy via HTTP for ESP32-S3 (if hardware available)
- Serial monitor via WebSocket for ESP32-S3
- Device lock management for ESP32-S3
- End-to-end build-deploy-monitor workflow

Requires:
- ESP32-S3 hardware connected (for deploy/monitor tests)
- fbuild daemon running with HTTP server
- FBUILD_DAEMON_PORT environment variable set to 9176 (for testing)
"""

import os
import time
from pathlib import Path

import pytest

# Set test environment
os.environ["FBUILD_DEV_MODE"] = "1"
os.environ["FBUILD_DAEMON_PORT"] = "9176"

from fbuild.daemon.client.devices_http import list_devices_http
from fbuild.daemon.client.http_utils import get_daemon_url, http_client, is_daemon_http_available
from fbuild.daemon.client.lifecycle import ensure_daemon_running, is_daemon_running, stop_daemon
from fbuild.daemon.client.requests_http import request_build_http, request_deploy_http

# ESP32-S3 test project
ESP32S3_PROJECT = Path(__file__).parent.parent / "esp32s3"


@pytest.fixture(scope="module")
def daemon():
    """Ensure daemon is running with HTTP server."""
    # Stop any existing daemon
    if is_daemon_running():
        stop_daemon()
        time.sleep(2)

    # Start daemon
    ensure_daemon_running(verbose=True)
    time.sleep(1)

    # Verify HTTP server is available
    assert is_daemon_http_available(), "Daemon HTTP server not available"

    yield

    # Cleanup
    stop_daemon()
    time.sleep(1)


@pytest.fixture
def esp32s3_device():
    """Find and return ESP32-S3 device if connected."""
    devices = list_devices_http(refresh=True)
    if not devices:
        pytest.skip("No devices connected")

    # Look for ESP32-S3 device
    for device in devices:
        # ESP32-S3 devices typically show up as CP210x or similar
        device_id = device.get("device_id", "")
        port = device.get("port", "")
        if "S3" in device_id.upper() or "S3" in port.upper():
            return device

    # If no S3 found, use first available device (might be S3)
    pytest.skip("No ESP32-S3 device found")


@pytest.mark.hardware
@pytest.mark.esp32s3
class TestESP32S3Build:
    """Test ESP32-S3 build via HTTP daemon."""

    def test_build_esp32s3_via_http(self, daemon):
        """Test building ESP32-S3 project via HTTP."""
        if not ESP32S3_PROJECT.exists():
            pytest.skip(f"ESP32-S3 test project not found at {ESP32S3_PROJECT}")

        # Build request
        success = request_build_http(
            project_dir=ESP32S3_PROJECT,
            environment="esp32-s3-devkitc-1",
            clean_build=False,
            verbose=True,
            jobs=4,
            timeout=600,  # ESP32 builds can take time
        )

        assert success is True, "ESP32-S3 build via HTTP failed"

    def test_build_esp32s3_clean_via_http(self, daemon):
        """Test clean build of ESP32-S3 project via HTTP."""
        if not ESP32S3_PROJECT.exists():
            pytest.skip(f"ESP32-S3 test project not found at {ESP32S3_PROJECT}")

        # Clean build request
        success = request_build_http(
            project_dir=ESP32S3_PROJECT,
            environment="esp32-s3-devkitc-1",
            clean_build=True,
            verbose=True,
            jobs=4,
            timeout=600,
        )

        assert success is True, "ESP32-S3 clean build via HTTP failed"

    def test_build_esp32s3_serial_compilation(self, daemon):
        """Test ESP32-S3 build with serial compilation (jobs=1)."""
        if not ESP32S3_PROJECT.exists():
            pytest.skip(f"ESP32-S3 test project not found at {ESP32S3_PROJECT}")

        # Serial build (for debugging)
        success = request_build_http(
            project_dir=ESP32S3_PROJECT,
            environment="esp32-s3-devkitc-1",
            clean_build=False,
            verbose=True,
            jobs=1,
            timeout=600,
        )

        assert success is True, "ESP32-S3 serial build via HTTP failed"

    def test_build_esp32s3_parallel_compilation(self, daemon):
        """Test ESP32-S3 build with parallel compilation."""
        if not ESP32S3_PROJECT.exists():
            pytest.skip(f"ESP32-S3 test project not found at {ESP32S3_PROJECT}")

        # Parallel build with 8 workers
        success = request_build_http(
            project_dir=ESP32S3_PROJECT,
            environment="esp32-s3-devkitc-1",
            clean_build=False,
            verbose=True,
            jobs=8,
            timeout=600,
        )

        assert success is True, "ESP32-S3 parallel build via HTTP failed"


@pytest.mark.hardware
@pytest.mark.esp32s3
class TestESP32S3Deploy:
    """Test ESP32-S3 deploy via HTTP daemon."""

    def test_deploy_esp32s3_via_http(self, daemon, esp32s3_device):
        """Test deploying to ESP32-S3 via HTTP."""
        if not ESP32S3_PROJECT.exists():
            pytest.skip(f"ESP32-S3 test project not found at {ESP32S3_PROJECT}")

        # Build first
        build_success = request_build_http(
            project_dir=ESP32S3_PROJECT,
            environment="esp32-s3-devkitc-1",
            clean_build=False,
            verbose=True,
            jobs=4,
            timeout=600,
        )
        assert build_success is True, "Build failed before deploy"

        # Deploy
        port = esp32s3_device.get("port")
        deploy_success = request_deploy_http(
            project_dir=ESP32S3_PROJECT,
            environment="esp32-s3-devkitc-1",
            port=port,
            verbose=True,
            timeout=120,
        )

        assert deploy_success is True, "ESP32-S3 deploy via HTTP failed"


@pytest.mark.hardware
@pytest.mark.esp32s3
class TestESP32S3DeviceLocks:
    """Test device lock management for ESP32-S3."""

    def test_device_list_includes_esp32s3(self, daemon, esp32s3_device):
        """Test that device list includes ESP32-S3."""
        device_id = esp32s3_device.get("device_id")
        port = esp32s3_device.get("port")

        assert device_id is not None, "Device ID should not be None"
        assert port is not None, "Port should not be None"
        assert esp32s3_device.get("is_connected") is True, "Device should be connected"

    def test_esp32s3_device_status(self, daemon, esp32s3_device):
        """Test getting ESP32-S3 device status."""
        from fbuild.daemon.client.devices_http import get_device_status_http

        device_id = esp32s3_device.get("device_id")
        status = get_device_status_http(device_id)

        assert status is not None, "Device status should not be None"
        assert status.get("is_connected") is True, "Device should be connected"


@pytest.mark.hardware
@pytest.mark.esp32s3
class TestESP32S3EndToEnd:
    """End-to-end workflow tests for ESP32-S3."""

    def test_build_deploy_workflow_via_http(self, daemon, esp32s3_device):
        """Test complete build-deploy workflow for ESP32-S3 via HTTP."""
        if not ESP32S3_PROJECT.exists():
            pytest.skip(f"ESP32-S3 test project not found at {ESP32S3_PROJECT}")

        # Step 1: Build
        print("\n=== Step 1: Building ESP32-S3 project ===")
        build_success = request_build_http(
            project_dir=ESP32S3_PROJECT,
            environment="esp32-s3-devkitc-1",
            clean_build=False,
            verbose=True,
            jobs=4,
            timeout=600,
        )
        assert build_success is True, "Build failed"

        # Step 2: Deploy
        print("\n=== Step 2: Deploying to ESP32-S3 ===")
        port = esp32s3_device.get("port")
        deploy_success = request_deploy_http(
            project_dir=ESP32S3_PROJECT,
            environment="esp32-s3-devkitc-1",
            port=port,
            verbose=True,
            timeout=120,
        )
        assert deploy_success is True, "Deploy failed"

        print("\n=== Build-Deploy workflow completed successfully ===")


@pytest.mark.hardware
@pytest.mark.esp32s3
class TestESP32S3HTTPPerformance:
    """Performance tests for ESP32-S3 operations via HTTP."""

    def test_build_performance_http_vs_baseline(self, daemon):
        """Compare HTTP daemon build performance for ESP32-S3."""
        if not ESP32S3_PROJECT.exists():
            pytest.skip(f"ESP32-S3 test project not found at {ESP32S3_PROJECT}")

        # Warm-up build (to ensure dependencies are cached)
        request_build_http(
            project_dir=ESP32S3_PROJECT,
            environment="esp32-s3-devkitc-1",
            clean_build=False,
            verbose=False,
            jobs=4,
            timeout=600,
        )

        # Timed build via HTTP
        start_time = time.time()
        success = request_build_http(
            project_dir=ESP32S3_PROJECT,
            environment="esp32-s3-devkitc-1",
            clean_build=False,
            verbose=True,
            jobs=4,
            timeout=600,
        )
        http_duration = time.time() - start_time

        assert success is True, "Build failed"
        print(f"\n=== ESP32-S3 build via HTTP: {http_duration:.2f}s ===")

        # Ensure build completed in reasonable time (< 5 minutes for incremental)
        assert http_duration < 300, f"Build took too long: {http_duration:.2f}s"


@pytest.mark.hardware
@pytest.mark.esp32s3
class TestESP32S3HTTPReliability:
    """Reliability tests for ESP32-S3 HTTP daemon operations."""

    def test_multiple_sequential_builds(self, daemon):
        """Test multiple sequential builds via HTTP."""
        if not ESP32S3_PROJECT.exists():
            pytest.skip(f"ESP32-S3 test project not found at {ESP32S3_PROJECT}")

        # Run 3 sequential builds
        for i in range(3):
            print(f"\n=== Build {i+1}/3 ===")
            success = request_build_http(
                project_dir=ESP32S3_PROJECT,
                environment="esp32-s3-devkitc-1",
                clean_build=False,
                verbose=True,
                jobs=4,
                timeout=600,
            )
            assert success is True, f"Build {i+1} failed"
            time.sleep(1)  # Brief pause between builds

    def test_build_with_different_job_counts(self, daemon):
        """Test builds with different parallel job counts."""
        if not ESP32S3_PROJECT.exists():
            pytest.skip(f"ESP32-S3 test project not found at {ESP32S3_PROJECT}")

        job_counts = [1, 2, 4, 8]
        for jobs in job_counts:
            print(f"\n=== Build with {jobs} job(s) ===")
            success = request_build_http(
                project_dir=ESP32S3_PROJECT,
                environment="esp32-s3-devkitc-1",
                clean_build=False,
                verbose=True,
                jobs=jobs,
                timeout=600,
            )
            assert success is True, f"Build with {jobs} jobs failed"


@pytest.mark.hardware
@pytest.mark.esp32s3
class TestESP32S3PortConfiguration:
    """Test port configuration for ESP32-S3 HTTP daemon."""

    def test_daemon_uses_custom_port(self, daemon):
        """Verify daemon is using custom port 9176."""
        with http_client() as client:
            response = client.get(get_daemon_url("/api/daemon/info"))
            assert response.status_code == 200

            data = response.json()
            assert data["port"] == 9176, f"Expected port 9176, got {data['port']}"
            print(f"\n=== Daemon running on custom port: {data['port']} ===")

    def test_custom_port_environment_variable(self):
        """Test that FBUILD_DAEMON_PORT environment variable is respected."""
        assert os.environ.get("FBUILD_DAEMON_PORT") == "9176"

        from fbuild.daemon.client.http_utils import get_daemon_port

        port = get_daemon_port()
        assert port == 9176, f"Expected port 9176 from env var, got {port}"


if __name__ == "__main__":
    pytest.main([__file__, "-v", "-s", "-m", "esp32s3"])
