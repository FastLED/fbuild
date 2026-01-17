"""
Same-port deploy tests for verifying port locking behavior.

These tests verify that:
1. Two deploys to the same port correctly conflict
2. Deploy with --monitor blocks subsequent operations on same port
3. Monitor to same port as existing monitor fails

Requires 1 ESP32-C6 device for hardware tests.
"""

import threading
import time
from typing import Any
from unittest.mock import MagicMock

import pytest

pytestmark = [pytest.mark.concurrent]


class TestSamePortDeployConflict:
    """Tests for same-port deploy conflicts (no hardware required)."""

    def test_two_deploys_same_port_second_fails(
        self,
        lock_manager: Any,
    ) -> None:
        """Two concurrent deploys to same port - second should fail.

        Using non-blocking lock acquisition (current daemon behavior),
        the second deploy attempt should fail immediately.
        """
        port = "COM3"
        results: dict[str, Any] = {}
        errors: dict[str, Exception] = {}

        def deploy1() -> None:
            with lock_manager.acquire_port_lock(port, blocking=True):
                results["deploy1_started"] = True
                time.sleep(0.5)  # Simulate deploy time
                results["deploy1_completed"] = True

        def deploy2() -> None:
            time.sleep(0.1)  # Let deploy1 start first
            try:
                with lock_manager.acquire_port_lock(port, blocking=False):
                    results["deploy2_acquired"] = True
            except RuntimeError as e:
                errors["deploy2"] = e
                results["deploy2_failed"] = True

        t1 = threading.Thread(target=deploy1)
        t2 = threading.Thread(target=deploy2)

        t1.start()
        t2.start()
        t1.join(timeout=5)
        t2.join(timeout=5)

        # Deploy 1 should succeed
        assert results.get("deploy1_completed") is True

        # Deploy 2 should fail with RuntimeError
        assert results.get("deploy2_failed") is True
        assert "deploy2" in errors
        assert isinstance(errors["deploy2"], RuntimeError)

    def test_deploy_error_message_includes_port(
        self,
        lock_manager: Any,
    ) -> None:
        """Error message should clearly identify which port is locked."""
        port = "COM7"

        with lock_manager.acquire_port_lock(port, blocking=True):
            with pytest.raises(RuntimeError) as exc_info:
                with lock_manager.acquire_port_lock(port, blocking=False):
                    pass

        assert port in str(exc_info.value) or "unavailable" in str(exc_info.value).lower()


class TestDeployMonitorConflict:
    """Tests for deploy + monitor port conflicts (no hardware required)."""

    def test_deploy_then_monitor_same_port_conflict(
        self,
        lock_manager: Any,
    ) -> None:
        """Deploy holds port lock, separate monitor to same port should fail."""
        port = "COM3"
        results: dict[str, Any] = {}
        errors: dict[str, Exception] = {}

        def deploy_with_monitoring() -> None:
            # Simulate deploy holding port lock for extended time (monitoring)
            with lock_manager.acquire_port_lock(port, blocking=True):
                results["deploy_acquired"] = True
                time.sleep(1.0)  # Simulate monitoring
                results["deploy_done"] = True

        def separate_monitor() -> None:
            time.sleep(0.2)  # Let deploy acquire first
            try:
                with lock_manager.acquire_port_lock(port, blocking=False):
                    results["monitor_acquired"] = True
            except RuntimeError as e:
                errors["monitor"] = e
                results["monitor_failed"] = True

        t1 = threading.Thread(target=deploy_with_monitoring)
        t2 = threading.Thread(target=separate_monitor)

        t1.start()
        t2.start()
        t1.join(timeout=5)
        t2.join(timeout=5)

        # Deploy should succeed
        assert results.get("deploy_done") is True

        # Separate monitor should fail
        assert results.get("monitor_failed") is True
        assert "monitor" in errors

    def test_monitor_monitor_same_port_conflict(
        self,
        lock_manager: Any,
    ) -> None:
        """Two monitors on same port - second should fail."""
        port = "/dev/ttyUSB0"
        results: dict[str, Any] = {}
        errors: dict[str, Exception] = {}

        def monitor1() -> None:
            with lock_manager.acquire_port_lock(port, blocking=True):
                results["monitor1_started"] = True
                time.sleep(0.5)
                results["monitor1_done"] = True

        def monitor2() -> None:
            time.sleep(0.1)
            try:
                with lock_manager.acquire_port_lock(port, blocking=False):
                    results["monitor2_acquired"] = True
            except RuntimeError as e:
                errors["monitor2"] = e
                results["monitor2_failed"] = True

        t1 = threading.Thread(target=monitor1)
        t2 = threading.Thread(target=monitor2)

        t1.start()
        t2.start()
        t1.join(timeout=5)
        t2.join(timeout=5)

        assert results.get("monitor1_done") is True
        assert results.get("monitor2_failed") is True


class TestDifferentPortOperations:
    """Tests for operations on different ports."""

    def test_two_deploys_different_ports_both_succeed(
        self,
        lock_manager: Any,
    ) -> None:
        """Two concurrent deploys to different ports should both succeed."""
        port1 = "COM3"
        port2 = "COM4"
        results: dict[str, bool] = {}

        def deploy_port1() -> None:
            with lock_manager.acquire_port_lock(port1, blocking=True):
                time.sleep(0.2)
                results["port1"] = True

        def deploy_port2() -> None:
            with lock_manager.acquire_port_lock(port2, blocking=True):
                time.sleep(0.2)
                results["port2"] = True

        t1 = threading.Thread(target=deploy_port1)
        t2 = threading.Thread(target=deploy_port2)

        start = time.time()
        t1.start()
        t2.start()
        t1.join(timeout=5)
        t2.join(timeout=5)
        elapsed = time.time() - start

        # Both should succeed
        assert results.get("port1") is True
        assert results.get("port2") is True

        # Should complete in parallel
        assert elapsed < 0.4

    def test_monitor_and_deploy_different_ports_both_succeed(
        self,
        lock_manager: Any,
    ) -> None:
        """Monitor on one port and deploy on another should both succeed."""
        monitor_port = "COM3"
        deploy_port = "COM4"
        results: dict[str, bool] = {}

        def monitor() -> None:
            with lock_manager.acquire_port_lock(monitor_port, blocking=True):
                time.sleep(0.2)
                results["monitor"] = True

        def deploy() -> None:
            with lock_manager.acquire_port_lock(deploy_port, blocking=True):
                time.sleep(0.2)
                results["deploy"] = True

        t1 = threading.Thread(target=monitor)
        t2 = threading.Thread(target=deploy)

        t1.start()
        t2.start()
        t1.join(timeout=5)
        t2.join(timeout=5)

        assert results.get("monitor") is True
        assert results.get("deploy") is True


class TestDeployProcessorLocking:
    """Tests for deploy processor lock behavior using mock context."""

    def test_deploy_processor_requires_project_and_port_locks(
        self,
        mock_daemon_context: Any,
    ) -> None:
        """DeployRequestProcessor should require both project and port locks."""
        from fbuild.daemon.processors.deploy_processor import DeployRequestProcessor

        processor = DeployRequestProcessor()

        # Create mock request
        mock_request = MagicMock()
        mock_request.project_dir = "/test/project"
        mock_request.environment = "esp32c6"
        mock_request.port = "COM3"
        mock_request.request_id = "test_123"

        # Get required locks
        locks = processor.get_required_locks(mock_request, mock_daemon_context)

        # Should require both project and port locks
        assert "project" in locks
        assert locks["project"] == "/test/project"
        assert "port" in locks
        assert locks["port"] == "COM3"

    def test_deploy_processor_without_port_only_requires_project_lock(
        self,
        mock_daemon_context: Any,
    ) -> None:
        """DeployRequestProcessor without port should only require project lock."""
        from fbuild.daemon.processors.deploy_processor import DeployRequestProcessor

        processor = DeployRequestProcessor()

        # Create mock request without port
        mock_request = MagicMock()
        mock_request.project_dir = "/test/project"
        mock_request.environment = "esp32c6"
        mock_request.port = None
        mock_request.request_id = "test_123"

        # Get required locks
        locks = processor.get_required_locks(mock_request, mock_daemon_context)

        # Should only require project lock
        assert "project" in locks
        assert "port" not in locks or locks.get("port") is None


class TestMonitorProcessorLocking:
    """Tests for monitor processor lock behavior."""

    def test_monitor_processor_requires_only_port_lock(
        self,
        mock_daemon_context: Any,
    ) -> None:
        """MonitorRequestProcessor should only require port lock."""
        from fbuild.daemon.processors.monitor_processor import MonitorRequestProcessor

        processor = MonitorRequestProcessor()

        # Create mock request
        mock_request = MagicMock()
        mock_request.project_dir = "/test/project"
        mock_request.environment = "esp32c6"
        mock_request.port = "COM3"
        mock_request.request_id = "test_123"

        # Get required locks
        locks = processor.get_required_locks(mock_request, mock_daemon_context)

        # Should only require port lock
        assert "port" in locks
        assert locks["port"] == "COM3"
        assert "project" not in locks


@pytest.mark.hardware
@pytest.mark.single_device
class TestHardwareDeployConflicts:
    """Hardware tests requiring an ESP32-C6 device.

    These tests spawn actual fbuild processes and verify concurrent behavior.
    """

    def test_two_deploys_same_device_one_fails(
        self,
        spawner: Any,
        esp32c6_project: Any,
    ) -> None:
        """Two deploys to same device - second should fail with port lock error.

        This test requires an actual ESP32-C6 device connected.
        """
        pytest.skip("Hardware test - requires ESP32-C6 device")

        # This would be the actual implementation:
        # proc1 = spawner.spawn_deploy(esp32c6_project, "esp32c6")
        # time.sleep(1)  # Let first deploy start
        # proc2 = spawner.spawn_deploy(esp32c6_project, "esp32c6")
        #
        # result1 = spawner.wait_and_get_output(proc1, timeout=120)
        # result2 = spawner.wait_and_get_output(proc2, timeout=10)
        #
        # # One should succeed, other should fail with port lock error
        # assert (result1.returncode == 0) or (result2.returncode == 0)
        # failing_result = result2 if result1.returncode == 0 else result1
        # assert "port" in failing_result.stdout.lower()
        # assert "in use" in failing_result.stdout.lower() or "locked" in failing_result.stdout.lower()

    def test_deploy_with_monitor_blocks_second_deploy(
        self,
        spawner: Any,
        esp32c6_project: Any,
    ) -> None:
        """Deploy with --monitor should block subsequent deploy attempts."""
        pytest.skip("Hardware test - requires ESP32-C6 device")

        # This would be the actual implementation:
        # proc1 = spawner.spawn_deploy(
        #     esp32c6_project, "esp32c6",
        #     monitor=True, monitor_timeout=30
        # )
        # spawner.wait_for_output(proc1, "Monitoring", timeout=60)
        #
        # # Try second deploy while first is monitoring
        # proc2 = spawner.spawn_deploy(esp32c6_project, "esp32c6")
        # result2 = spawner.wait_and_get_output(proc2, timeout=10)
        #
        # # Second should fail with port lock error
        # assert result2.returncode != 0
        # assert "port" in result2.stdout.lower()
