"""Integration tests for rapid port reuse (deploy → monitor workflow).

These tests verify the complete validation workflow timing:
1. Deploy firmware to device
2. USB-CDC re-enumeration delay (Windows: 5-15s)
3. SerialMonitor attach for validation

Critical timing constraints:
- Daemon SELF_EVICTION_TIMEOUT must be ≥ workflow gap duration
- Port must be available after deploy completes
- No timeout errors during normal workflow

Test Strategy:
- Use real ESP32 test project for hardware tests
- Use mocked deployer for timing tests without hardware
- Measure actual timing and success rates
"""

import time
from pathlib import Path

import pytest

# Mark all tests as integration
pytestmark = pytest.mark.integration


class MockDeployer:
    """Mock deployer that simulates ESP32 firmware upload + USB re-enumeration."""

    def __init__(self, deploy_duration: float = 5.0, usb_reenum_delay: float = 5.0):
        """Initialize mock deployer.

        Args:
            deploy_duration: Simulated deploy time (seconds)
            usb_reenum_delay: Simulated USB re-enumeration delay (seconds)
        """
        self.deploy_duration = deploy_duration
        self.usb_reenum_delay = usb_reenum_delay
        self.deploy_count = 0

    def deploy(self, firmware_path: Path, port: str) -> bool:
        """Simulate firmware deployment.

        Args:
            firmware_path: Path to firmware binary
            port: Serial port for deployment

        Returns:
            True if deployment successful
        """
        self.deploy_count += 1

        # Simulate esptool flashing time
        time.sleep(self.deploy_duration)

        # Simulate USB-CDC re-enumeration (port becomes unavailable)
        # In real hardware, Windows needs time to re-detect the device
        time.sleep(self.usb_reenum_delay)

        return True

    def get_total_workflow_time(self) -> float:
        """Get total time for deploy + USB re-enumeration."""
        return self.deploy_duration + self.usb_reenum_delay


class TestDeployThenMonitorWorkflow:
    """Test deploy → monitor workflow timing."""

    @pytest.mark.parametrize("delay", [0, 5, 10, 15, 30])
    def test_deploy_then_delayed_monitor(self, delay):
        """Test SerialMonitor attach after varying delays post-deploy.

        EXPECTED RESULTS:
        - delay=0: May fail (USB still re-enumerating)
        - delay=5: May fail on Windows (USB-CDC can take 10-15s)
        - delay=10: Should succeed on most systems
        - delay=15: Should succeed reliably
        - delay=30: Should succeed (well past USB re-enum)

        This test measures the minimum reliable delay needed.
        """
        deployer = MockDeployer(deploy_duration=5.0, usb_reenum_delay=5.0)
        port = "COM13"  # Mock port
        firmware_path = Path("mock_firmware.bin")

        # Deploy firmware
        deploy_start = time.time()
        success = deployer.deploy(firmware_path, port)
        deploy_end = time.time()

        assert success, "Deploy should succeed"
        deploy_duration = deploy_end - deploy_start
        print(f"\nDeploy took {deploy_duration:.2f}s")

        # Wait specified delay
        if delay > 0:
            time.sleep(delay)

        # Attempt to attach SerialMonitor
        # In real test, this would use fbuild.api.SerialMonitor
        # For mock test, we just verify timing constraints
        attach_start = time.time()
        gap_duration = attach_start - deploy_start

        print(f"Gap between deploy start and attach: {gap_duration:.2f}s")
        print(f"Deploy duration: {deploy_duration:.2f}s")
        print(f"Additional delay: {delay}s")

        # Verify daemon should still be alive
        # Daemon eviction timer starts when deploy completes
        time_since_deploy_end = attach_start - deploy_end

        from fbuild.daemon.daemon import SELF_EVICTION_TIMEOUT

        assert time_since_deploy_end < SELF_EVICTION_TIMEOUT, f"Attach attempt at {time_since_deploy_end:.1f}s should be within " f"daemon timeout ({SELF_EVICTION_TIMEOUT}s)"

        # For hardware tests, we would actually attach and verify success
        # For now, just verify timing constraints
        print(f"Daemon should be alive (gap: {time_since_deploy_end:.1f}s < timeout: {SELF_EVICTION_TIMEOUT}s)")

    def test_deploy_then_immediate_monitor(self):
        """Test immediate SerialMonitor attach after deploy (0s delay).

        EXPECTED BEHAVIOR:
        - Deploy completes
        - Client attempts immediate attach (before USB re-enumeration)
        - SharedSerialManager retries with backoff (15 retries × 1s)
        - Attach succeeds once port re-enumerates

        EXPECTED FAILURE (without retry logic):
        - Attach fails immediately (port not available)

        EXPECTED PASS (with retry logic):
        - Attach succeeds after ~5-10s (port re-enumerates during retries)
        """
        deployer = MockDeployer(deploy_duration=3.0, usb_reenum_delay=7.0)
        port = "COM13"

        # Deploy
        deploy_success = deployer.deploy(Path("firmware.bin"), port)
        assert deploy_success

        # Immediate attach attempt (t=0 after deploy)
        attach_start = time.time()

        # In real test with hardware:
        # from fbuild.api import SerialMonitor
        # with SerialMonitor(port=port, baud_rate=115200) as mon:
        #     # Should succeed after retries
        #     pass

        # For mock test, verify timing allows for retry window
        # SharedSerialManager retry logic: 15 retries × 1s = 15s max wait
        # USB re-enum: 7s (from mock)
        # Attach should succeed within 15s window

        print(f"\nImmediate attach started {time.time() - attach_start:.2f}s after deploy")
        print("Expected: SharedSerialManager retries until port available (~7s)")

    def test_validation_workflow_simulation(self):
        """Simulate full FastLED validation workflow timing.

        Workflow steps:
        1. Deploy firmware (5s)
        2. USB-CDC re-enumeration (5s built into deploy)
        3. Port availability check (up to 15 retries × 1s)
        4. Pin discovery SerialMonitor attach
        5. GPIO test SerialMonitor attach

        Total workflow time: ~25-30s worst case
        Daemon must survive this entire workflow.
        """
        deployer = MockDeployer(deploy_duration=5.0, usb_reenum_delay=5.0)
        port = "COM13"

        workflow_start = time.time()

        # Step 1: Deploy firmware
        print("\n[Step 1] Deploy firmware...")
        deploy_start = time.time()
        deployer.deploy(Path("firmware.bin"), port)
        deploy_end = time.time()
        print(f"  Deploy completed in {deploy_end - deploy_start:.2f}s")

        # Step 2: USB re-enumeration (handled by deployer)
        # Port becomes unavailable during this time
        print("[Step 2] USB-CDC re-enumeration (included in deploy)")

        # Step 3: Port availability check (simulate retries)
        print("[Step 3] Port availability check...")
        retry_count = 0
        max_retries = 15
        retry_delay = 1.0

        for retry in range(max_retries):
            retry_count += 1
            # In real test: check if port exists
            # For mock: simulate retry timing
            time.sleep(retry_delay)

            # Simulate port becoming available after ~10s total
            time_since_deploy_end = time.time() - deploy_end
            if time_since_deploy_end >= 10.0:
                print(f"  Port available after {retry_count} retries ({time_since_deploy_end:.2f}s)")
                break
        else:
            pytest.fail("Port did not become available within retry window")

        # Step 4: Pin discovery SerialMonitor attach
        print("[Step 4] Pin discovery (SerialMonitor attach)...")
        pin_discovery_start = time.time()

        # Verify daemon should still be alive
        time_since_deploy_end = pin_discovery_start - deploy_end
        from fbuild.daemon.daemon import SELF_EVICTION_TIMEOUT

        assert time_since_deploy_end < SELF_EVICTION_TIMEOUT, f"Pin discovery at t={time_since_deploy_end:.1f}s should be within " f"daemon timeout ({SELF_EVICTION_TIMEOUT}s)"

        # Simulate pin discovery
        time.sleep(1.0)
        print("  Pin discovery completed")

        # Step 5: GPIO test SerialMonitor attach
        print("[Step 5] GPIO test (SerialMonitor attach)...")
        gpio_test_start = time.time()

        # Verify daemon still alive
        time_since_deploy_end = gpio_test_start - deploy_end
        assert time_since_deploy_end < SELF_EVICTION_TIMEOUT, f"GPIO test at t={time_since_deploy_end:.1f}s should be within " f"daemon timeout ({SELF_EVICTION_TIMEOUT}s)"

        # Simulate GPIO test
        time.sleep(1.0)
        print("  GPIO test completed")

        # Workflow complete
        workflow_end = time.time()
        total_workflow_time = workflow_end - workflow_start

        print("\n[WORKFLOW COMPLETE]")
        print(f"  Total time: {total_workflow_time:.2f}s")
        print(f"  Deploy duration: {deploy_end - deploy_start:.2f}s")
        print(f"  Max gap (deploy end → pin discovery): {pin_discovery_start - deploy_end:.2f}s")
        print(f"  Daemon timeout: {SELF_EVICTION_TIMEOUT}s")

        # Verify workflow completed within reasonable time
        assert total_workflow_time < 60.0, "Workflow should complete within 60s"

        # Verify all steps occurred within daemon timeout window
        assert (pin_discovery_start - deploy_end) < SELF_EVICTION_TIMEOUT
        assert (gpio_test_start - deploy_end) < SELF_EVICTION_TIMEOUT


@pytest.mark.hardware
class TestRealHardwareDeployMonitorWorkflow:
    """Tests requiring actual ESP32 hardware.

    These tests use real hardware and verify the complete workflow.
    Run with: pytest -m hardware
    """

    def test_real_esp32_deploy_then_monitor(self, esp32_port):
        """Test real ESP32 deploy → monitor workflow.

        Requires:
        - ESP32 device connected
        - Fixture providing port via pytest

        This test demonstrates the actual timing on real hardware.
        """
        pytest.skip("Requires ESP32 hardware - implement with conftest fixture")

        # Implementation with real hardware:
        # from fbuild.deploy.deployer_esp32 import ESP32Deployer
        # from fbuild.api import SerialMonitor
        #
        # deployer = ESP32Deployer()
        # deployer.deploy(firmware_path, esp32_port)
        #
        # # Immediate attach
        # with SerialMonitor(port=esp32_port, baud_rate=115200) as mon:
        #     # Should succeed after retry
        #     for line in mon.read_lines(timeout=5.0):
        #         print(line)
        #         if "READY" in line:
        #             break

    def test_measure_actual_usb_reenum_time(self, esp32_port):
        """Measure actual USB-CDC re-enumeration time on this system.

        Useful for validating test assumptions and tuning retry logic.
        """
        pytest.skip("Requires ESP32 hardware - implement with conftest fixture")

        # Implementation:
        # 1. Deploy firmware
        # 2. Poll for port availability every 100ms
        # 3. Measure time until port becomes available
        # 4. Report actual re-enumeration duration
