"""
Lock persistence tests for verifying lock lifetime through operations.

These tests verify that:
1. Deploy holds project lock during build phase
2. Deploy holds port lock during upload phase
3. Deploy with --monitor holds locks throughout entire operation
4. Locks are released after monitor timeout

Requires 1 ESP32-C6 device for hardware tests.
"""

import threading
import time
from contextlib import ExitStack
from typing import Any

import pytest

pytestmark = [pytest.mark.concurrent]


class TestDeployLockPersistenceDuringBuild:
    """Tests for project lock persistence during deploy build phase."""

    def test_deploy_holds_project_lock_during_build_phase(
        self,
        lock_manager: Any,
    ) -> None:
        """During deploy's build phase, project lock should block other builds."""
        project_dir = "/test/project"
        results: dict[str, Any] = {}
        errors: dict[str, Exception] = {}

        def deploy_operation() -> None:
            """Simulate a deploy that holds project lock during build."""
            with lock_manager.acquire_project_lock(project_dir, blocking=True):
                results["deploy_acquired_project"] = True
                # Simulate build phase (takes time)
                time.sleep(0.5)
                results["deploy_build_done"] = True
                # Simulate upload phase
                time.sleep(0.2)
                results["deploy_upload_done"] = True

        def concurrent_build() -> None:
            """Try to build during deploy's build phase."""
            time.sleep(0.2)  # Let deploy start build phase
            try:
                with lock_manager.acquire_project_lock(project_dir, blocking=False):
                    results["concurrent_build_acquired"] = True
            except RuntimeError as e:
                errors["concurrent_build"] = e
                results["concurrent_build_failed"] = True

        t1 = threading.Thread(target=deploy_operation)
        t2 = threading.Thread(target=concurrent_build)

        t1.start()
        t2.start()
        t1.join(timeout=5)
        t2.join(timeout=5)

        # Deploy should complete
        assert results.get("deploy_upload_done") is True

        # Concurrent build should have failed
        assert results.get("concurrent_build_failed") is True
        assert "concurrent_build" in errors


class TestDeployLockPersistenceDuringUpload:
    """Tests for port lock persistence during deploy upload phase."""

    def test_deploy_holds_port_lock_during_upload_phase(
        self,
        lock_manager: Any,
    ) -> None:
        """During deploy's upload phase, port lock should block monitor."""
        port = "COM3"
        results: dict[str, Any] = {}
        errors: dict[str, Exception] = {}

        def deploy_operation() -> None:
            """Simulate deploy holding port during upload."""
            with lock_manager.acquire_port_lock(port, blocking=True):
                results["deploy_acquired_port"] = True
                # Simulate upload phase
                time.sleep(0.5)
                results["deploy_upload_done"] = True

        def concurrent_monitor() -> None:
            """Try to monitor during deploy's upload phase."""
            time.sleep(0.2)  # Let deploy start upload
            try:
                with lock_manager.acquire_port_lock(port, blocking=False):
                    results["monitor_acquired"] = True
            except RuntimeError as e:
                errors["monitor"] = e
                results["monitor_failed"] = True

        t1 = threading.Thread(target=deploy_operation)
        t2 = threading.Thread(target=concurrent_monitor)

        t1.start()
        t2.start()
        t1.join(timeout=5)
        t2.join(timeout=5)

        assert results.get("deploy_upload_done") is True
        assert results.get("monitor_failed") is True


class TestDeployWithMonitorLockPersistence:
    """Tests for lock persistence through deploy + monitor cycle."""

    def test_deploy_with_monitor_holds_all_locks_throughout(
        self,
        lock_manager: Any,
    ) -> None:
        """Deploy with --monitor should hold both locks until monitor exits."""
        project_dir = "/test/project"
        port = "COM3"
        results: dict[str, Any] = {}
        errors: dict[str, Exception] = {}

        def deploy_with_monitor() -> None:
            """Simulate deploy with monitoring - holds locks entire time."""
            with ExitStack() as stack:
                stack.enter_context(lock_manager.acquire_project_lock(project_dir, blocking=True))
                results["project_lock_acquired"] = True

                stack.enter_context(lock_manager.acquire_port_lock(port, blocking=True))
                results["port_lock_acquired"] = True

                # Simulate build
                time.sleep(0.2)
                results["build_done"] = True

                # Simulate upload
                time.sleep(0.2)
                results["upload_done"] = True

                # Simulate monitoring
                time.sleep(0.5)
                results["monitor_done"] = True

        def try_second_deploy() -> None:
            """Try to deploy during first deploy's monitor phase."""
            time.sleep(0.5)  # Wait until first deploy is monitoring
            try:
                # Should fail on either project or port lock
                with ExitStack() as stack:
                    stack.enter_context(lock_manager.acquire_project_lock(project_dir, blocking=False))
                    stack.enter_context(lock_manager.acquire_port_lock(port, blocking=False))
                    results["second_deploy_acquired"] = True
            except RuntimeError as e:
                errors["second_deploy"] = e
                results["second_deploy_failed"] = True

        t1 = threading.Thread(target=deploy_with_monitor)
        t2 = threading.Thread(target=try_second_deploy)

        t1.start()
        t2.start()
        t1.join(timeout=5)
        t2.join(timeout=5)

        # First deploy should complete all phases
        assert results.get("monitor_done") is True

        # Second deploy should have failed
        assert results.get("second_deploy_failed") is True
        assert "second_deploy" in errors


class TestLockReleaseAfterMonitorTimeout:
    """Tests for lock release after monitor timeout."""

    def test_lock_released_after_monitor_timeout(
        self,
        lock_manager: Any,
    ) -> None:
        """After first deploy+monitor completes, second deploy should succeed."""
        project_dir = "/test/project"
        port = "COM3"
        results: dict[str, Any] = {}

        def first_deploy_with_monitor() -> None:
            """First deploy with short monitor timeout."""
            with ExitStack() as stack:
                stack.enter_context(lock_manager.acquire_project_lock(project_dir, blocking=True))
                stack.enter_context(lock_manager.acquire_port_lock(port, blocking=True))

                # Quick operation
                time.sleep(0.3)
                results["first_deploy_done"] = True
            results["first_locks_released"] = True

        def second_deploy() -> None:
            """Second deploy after first completes."""
            time.sleep(0.5)  # Wait for first to complete
            with ExitStack() as stack:
                stack.enter_context(lock_manager.acquire_project_lock(project_dir, blocking=False))
                stack.enter_context(lock_manager.acquire_port_lock(port, blocking=False))
                results["second_deploy_acquired"] = True

        t1 = threading.Thread(target=first_deploy_with_monitor)
        t2 = threading.Thread(target=second_deploy)

        t1.start()
        t2.start()
        t1.join(timeout=5)
        t2.join(timeout=5)

        # First should complete
        assert results.get("first_deploy_done") is True
        assert results.get("first_locks_released") is True

        # Second should succeed after first releases locks
        assert results.get("second_deploy_acquired") is True

    def test_blocking_wait_for_lock_after_monitor(
        self,
        lock_manager: Any,
    ) -> None:
        """Second deploy with blocking=True should wait for monitor to finish."""
        project_dir = "/test/project"
        port = "COM3"
        results: dict[str, Any] = {}
        timings: dict[str, float] = {}

        def first_deploy() -> None:
            """First deploy holds locks for a bit."""
            timings["first_start"] = time.time()
            with ExitStack() as stack:
                stack.enter_context(lock_manager.acquire_project_lock(project_dir, blocking=True))
                stack.enter_context(lock_manager.acquire_port_lock(port, blocking=True))
                time.sleep(0.5)  # Hold locks
                timings["first_release"] = time.time()
                results["first_done"] = True

        def second_deploy() -> None:
            """Second deploy waits for locks."""
            time.sleep(0.1)  # Let first start
            timings["second_start_wait"] = time.time()
            with ExitStack() as stack:
                stack.enter_context(lock_manager.acquire_project_lock(project_dir, blocking=True))
                stack.enter_context(lock_manager.acquire_port_lock(port, blocking=True))
                timings["second_acquired"] = time.time()
                results["second_acquired"] = True

        t1 = threading.Thread(target=first_deploy)
        t2 = threading.Thread(target=second_deploy)

        t1.start()
        t2.start()
        t1.join(timeout=5)
        t2.join(timeout=5)

        # Both should succeed
        assert results.get("first_done") is True
        assert results.get("second_acquired") is True

        # Second should have waited for first
        wait_time = timings["second_acquired"] - timings["second_start_wait"]
        assert wait_time >= 0.3  # Should have waited


class TestLockPersistenceWithExceptions:
    """Tests for lock release when exceptions occur."""

    def test_project_lock_released_on_build_exception(
        self,
        lock_manager: Any,
    ) -> None:
        """Project lock should be released even if build throws exception."""
        project_dir = "/test/project"

        # Simulate build failure
        with pytest.raises(RuntimeError):
            with lock_manager.acquire_project_lock(project_dir, blocking=True):
                raise RuntimeError("Build failed!")

        # Lock should be released - next operation should succeed
        with lock_manager.acquire_project_lock(project_dir, blocking=False):
            pass

    def test_port_lock_released_on_upload_exception(
        self,
        lock_manager: Any,
    ) -> None:
        """Port lock should be released even if upload throws exception."""
        port = "COM3"

        # Simulate upload failure
        with pytest.raises(RuntimeError):
            with lock_manager.acquire_port_lock(port, blocking=True):
                raise RuntimeError("Upload failed!")

        # Lock should be released
        with lock_manager.acquire_port_lock(port, blocking=False):
            pass

    def test_both_locks_released_on_deploy_exception(
        self,
        lock_manager: Any,
    ) -> None:
        """Both project and port locks should be released on exception."""
        project_dir = "/test/project"
        port = "COM3"

        # Simulate deploy failure after acquiring both locks
        with pytest.raises(RuntimeError):
            with ExitStack() as stack:
                stack.enter_context(lock_manager.acquire_project_lock(project_dir, blocking=True))
                stack.enter_context(lock_manager.acquire_port_lock(port, blocking=True))
                raise RuntimeError("Deploy failed!")

        # Both locks should be released
        with lock_manager.acquire_project_lock(project_dir, blocking=False):
            pass
        with lock_manager.acquire_port_lock(port, blocking=False):
            pass


@pytest.mark.hardware
@pytest.mark.single_device
class TestHardwareLockPersistence:
    """Hardware tests for lock persistence requiring ESP32-C6 device."""

    def test_deploy_holds_lock_during_build(
        self,
        spawner: Any,
        esp32c6_project: Any,
    ) -> None:
        """During deploy's build phase, second build should fail."""
        pytest.skip("Hardware test - requires ESP32-C6 device")

    def test_deploy_holds_lock_during_upload(
        self,
        spawner: Any,
        esp32c6_project: Any,
    ) -> None:
        """During deploy's upload phase, second monitor should fail."""
        pytest.skip("Hardware test - requires ESP32-C6 device")

    def test_deploy_with_monitor_holds_lock_throughout(
        self,
        spawner: Any,
        esp32c6_project: Any,
    ) -> None:
        """Deploy with --monitor holds lock until monitor exits."""
        pytest.skip("Hardware test - requires ESP32-C6 device")

    def test_lock_released_after_monitor_timeout(
        self,
        spawner: Any,
        esp32c6_project: Any,
    ) -> None:
        """After monitor timeout, second deploy should succeed."""
        pytest.skip("Hardware test - requires ESP32-C6 device")
