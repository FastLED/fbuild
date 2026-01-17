"""
End-to-end single device integration tests.

These tests verify the complete concurrent safety workflow with a single
ESP32-C6 device. They test the interaction between locking, port state
tracking, and the full deploy/monitor cycle.

Requires 1 ESP32-C6 device for hardware tests.
"""

import threading
import time
from contextlib import ExitStack
from pathlib import Path
from typing import Any

import pytest

from fbuild.daemon.lock_manager import ResourceLockManager
from fbuild.daemon.port_state_manager import PortState, PortStateManager

pytestmark = [pytest.mark.concurrent, pytest.mark.single_device]


class TestE2ELockAndPortState:
    """End-to-end tests combining locks and port state (no hardware)."""

    def test_full_deploy_lifecycle_with_port_state(self) -> None:
        """Test complete deploy lifecycle with port state tracking.

        Simulates:
        1. Acquire project lock for build
        2. Track port state as UPLOADING during upload
        3. Transition to MONITORING state
        4. Release everything on completion
        """
        lock_manager = ResourceLockManager()
        port_state_manager = PortStateManager()

        project_dir = "/test/project"
        port = "COM3"
        client_pid = 12345

        # Simulate full deploy with monitoring
        with lock_manager.acquire_project_lock(project_dir, blocking=True):
            # Build phase - no port state yet
            time.sleep(0.1)  # Simulate build

            # Upload phase - acquire port lock and track state
            with lock_manager.acquire_port_lock(port, blocking=True):
                port_state_manager.acquire_port(
                    port=port,
                    state=PortState.UPLOADING,
                    client_pid=client_pid,
                    project_dir=project_dir,
                    environment="esp32c6",
                    operation_id="deploy_test",
                )

                # Verify state during upload
                info = port_state_manager.get_port_info(port)
                assert info is not None
                assert info.state == PortState.UPLOADING

                time.sleep(0.1)  # Simulate upload

                # Transition to monitoring
                port_state_manager.update_state(port, PortState.MONITORING)

                info = port_state_manager.get_port_info(port)
                assert info is not None
                assert info.state == PortState.MONITORING

                time.sleep(0.1)  # Simulate monitoring

                # Release port state
                port_state_manager.release_port(port)

        # Verify cleanup
        assert port_state_manager.is_port_available(port)
        assert lock_manager.get_lock_count()["port_locks"] == 1  # Lock exists but released
        assert lock_manager.get_lock_count()["project_locks"] == 1

    def test_concurrent_deploys_with_port_state(self) -> None:
        """Test two concurrent deploys with port state tracking.

        First deploy should succeed, second should fail immediately.
        """
        lock_manager = ResourceLockManager()
        port_state_manager = PortStateManager()

        project_dir = "/test/project"
        port = "COM3"

        results: dict[str, Any] = {}
        errors: dict[str, Exception] = {}

        def deploy1() -> None:
            with ExitStack() as stack:
                stack.enter_context(lock_manager.acquire_project_lock(project_dir, blocking=True))
                stack.enter_context(lock_manager.acquire_port_lock(port, blocking=True))

                port_state_manager.acquire_port(
                    port=port,
                    state=PortState.UPLOADING,
                    client_pid=1111,
                    project_dir=project_dir,
                    environment="esp32c6",
                    operation_id="deploy_1",
                )
                results["deploy1_started"] = True

                time.sleep(0.5)  # Hold locks

                port_state_manager.release_port(port)
                results["deploy1_done"] = True

        def deploy2() -> None:
            time.sleep(0.1)  # Let deploy1 start

            # Check port state before attempting
            info = port_state_manager.get_port_info(port)
            results["deploy2_saw_port_in_use"] = info is not None

            try:
                with ExitStack() as stack:
                    stack.enter_context(lock_manager.acquire_project_lock(project_dir, blocking=False))
                    stack.enter_context(lock_manager.acquire_port_lock(port, blocking=False))
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
        assert results.get("deploy1_done") is True

        # Deploy 2 should have seen port in use and failed
        assert results.get("deploy2_saw_port_in_use") is True
        assert results.get("deploy2_failed") is True

    def test_port_state_visible_during_monitoring(self) -> None:
        """Test that port state is visible while monitoring is active.

        Verifies clients can see what's happening on a port.
        """
        lock_manager = ResourceLockManager()
        port_state_manager = PortStateManager()

        port = "COM3"
        results: dict[str, Any] = {}

        def monitoring_operation() -> None:
            with lock_manager.acquire_port_lock(port, blocking=True):
                port_state_manager.acquire_port(
                    port=port,
                    state=PortState.MONITORING,
                    client_pid=12345,
                    project_dir="/test/project",
                    environment="esp32c6",
                    operation_id="monitor_test",
                )
                results["monitoring_started"] = True

                time.sleep(0.5)

                port_state_manager.release_port(port)
            results["monitoring_done"] = True

        def observer() -> None:
            time.sleep(0.2)  # Wait for monitoring to start

            info = port_state_manager.get_port_info(port)
            if info:
                results["observer_saw_state"] = info.state.value
                results["observer_saw_pid"] = info.client_pid
                results["observer_saw_project"] = info.project_dir

        t1 = threading.Thread(target=monitoring_operation)
        t2 = threading.Thread(target=observer)

        t1.start()
        t2.start()
        t1.join(timeout=5)
        t2.join(timeout=5)

        assert results.get("monitoring_done") is True
        assert results.get("observer_saw_state") == "monitoring"
        assert results.get("observer_saw_pid") == 12345
        assert results.get("observer_saw_project") == "/test/project"


class TestE2ELockReleaseScenarios:
    """End-to-end tests for lock release scenarios."""

    def test_lock_release_on_build_failure_clears_port_state(self) -> None:
        """On build failure, both locks and port state should be released."""
        lock_manager = ResourceLockManager()
        port_state_manager = PortStateManager()

        project_dir = "/test/project"
        port = "COM3"

        try:
            with ExitStack() as stack:
                stack.enter_context(lock_manager.acquire_project_lock(project_dir, blocking=True))
                stack.enter_context(lock_manager.acquire_port_lock(port, blocking=True))

                port_state_manager.acquire_port(
                    port=port,
                    state=PortState.UPLOADING,
                    client_pid=12345,
                    project_dir=project_dir,
                    environment="esp32c6",
                    operation_id="deploy_test",
                )

                # Simulate build failure
                raise RuntimeError("Build failed!")
        except RuntimeError:
            # Clean up port state on failure
            port_state_manager.release_port(port)

        # Verify cleanup
        assert port_state_manager.is_port_available(port)

        # Locks should be released - next acquire should succeed
        with lock_manager.acquire_project_lock(project_dir, blocking=False):
            pass
        with lock_manager.acquire_port_lock(port, blocking=False):
            pass

    def test_sequential_deploys_after_failure(self) -> None:
        """After a failed deploy, subsequent deploy should succeed."""
        lock_manager = ResourceLockManager()
        port_state_manager = PortStateManager()

        project_dir = "/test/project"
        port = "COM3"

        # First deploy fails
        try:
            with ExitStack() as stack:
                stack.enter_context(lock_manager.acquire_project_lock(project_dir, blocking=True))
                stack.enter_context(lock_manager.acquire_port_lock(port, blocking=True))

                port_state_manager.acquire_port(
                    port=port,
                    state=PortState.UPLOADING,
                    client_pid=12345,
                    project_dir=project_dir,
                    environment="esp32c6",
                    operation_id="deploy_1",
                )

                raise RuntimeError("Deploy failed!")
        except RuntimeError:
            port_state_manager.release_port(port)

        # Second deploy should succeed
        with ExitStack() as stack:
            stack.enter_context(lock_manager.acquire_project_lock(project_dir, blocking=False))
            stack.enter_context(lock_manager.acquire_port_lock(port, blocking=False))

            port_state_manager.acquire_port(
                port=port,
                state=PortState.UPLOADING,
                client_pid=67890,
                project_dir=project_dir,
                environment="esp32c6",
                operation_id="deploy_2",
            )

            info = port_state_manager.get_port_info(port)
            assert info is not None
            assert info.client_pid == 67890
            assert info.operation_id == "deploy_2"

            port_state_manager.release_port(port)


@pytest.mark.hardware
class TestE2EHardwareSingleDevice:
    """Hardware tests with a single ESP32-C6 device.

    These tests require an actual device connected.
    """

    def test_concurrent_deploy_monitor_single_device(
        self,
        spawner: Any,
        esp32c6_project: Path,
    ) -> None:
        """Test concurrent safety with a single ESP32-C6 device.

        Scenario:
        1. Start deploy with --monitor (timeout 10s)
        2. Wait 2s for build to complete and upload to start
        3. Try second deploy to same device
        4. Verify second deploy fails with clear error
        5. Wait for first monitor to timeout
        6. Try third deploy
        7. Verify third deploy succeeds
        """
        pytest.skip("Hardware test - requires ESP32-C6 device")

        # Implementation would be:
        # proc1 = spawner.spawn_deploy(
        #     esp32c6_project, "esp32c6",
        #     monitor=True, monitor_timeout=10
        # )
        #
        # # Wait for build and upload
        # spawner.wait_for_output(proc1, "Monitoring", timeout=60)
        #
        # # Try second deploy - should fail
        # proc2 = spawner.spawn_deploy(esp32c6_project, "esp32c6")
        # result2 = spawner.wait_and_get_output(proc2, timeout=10)
        # assert result2.returncode != 0
        # assert "in use" in result2.stdout.lower() or "locked" in result2.stdout.lower()
        #
        # # Wait for first to complete
        # result1 = spawner.wait_and_get_output(proc1, timeout=30)
        #
        # # Third deploy should succeed
        # proc3 = spawner.spawn_deploy(esp32c6_project, "esp32c6")
        # result3 = spawner.wait_and_get_output(proc3, timeout=120)
        # assert result3.returncode == 0

    def test_build_during_monitor_same_project(
        self,
        spawner: Any,
        esp32c6_project: Path,
    ) -> None:
        """Test that build fails when monitor is active on same project."""
        pytest.skip("Hardware test - requires ESP32-C6 device")

    def test_monitor_during_deploy_same_port(
        self,
        spawner: Any,
        esp32c6_project: Path,
    ) -> None:
        """Test that monitor fails when deploy is using the same port."""
        pytest.skip("Hardware test - requires ESP32-C6 device")

    def test_deploy_different_project_during_monitor(
        self,
        spawner: Any,
        esp32c6_project: Path,
        esp32dev_project: Path,
    ) -> None:
        """Test that deploying different project works during monitor.

        If using different port, should succeed.
        If same port, should fail.
        """
        pytest.skip("Hardware test - requires ESP32-C6 device and ESP32 Dev")
