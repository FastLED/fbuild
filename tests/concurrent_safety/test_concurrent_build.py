"""
Same-project build tests for verifying build locking behavior.

These tests verify that:
1. Two builds of the same project correctly conflict
2. Builds of different projects succeed in parallel
3. Build locks are released after completion

No hardware required - tests build locking at the daemon level.
"""

import threading
import time
from typing import Any
from unittest.mock import MagicMock

import pytest

pytestmark = pytest.mark.concurrent


class TestSameProjectBuildConflict:
    """Tests for same-project build conflicts."""

    def test_two_builds_same_project_second_fails_non_blocking(
        self,
        lock_manager: Any,
    ) -> None:
        """Two concurrent builds of the same project - second should fail.

        When using non-blocking lock acquisition (current daemon behavior),
        the second build attempt should fail immediately.
        """
        project_dir = "/test/project"
        results: dict[str, Any] = {}
        errors: dict[str, Exception] = {}

        def build1() -> None:
            with lock_manager.acquire_project_lock(project_dir, blocking=True):
                results["build1_started"] = True
                time.sleep(0.5)  # Simulate build time
                results["build1_completed"] = True

        def build2() -> None:
            time.sleep(0.1)  # Let build1 start first
            try:
                with lock_manager.acquire_project_lock(project_dir, blocking=False):
                    results["build2_acquired"] = True
            except RuntimeError as e:
                errors["build2"] = e
                results["build2_failed"] = True

        t1 = threading.Thread(target=build1)
        t2 = threading.Thread(target=build2)

        t1.start()
        t2.start()
        t1.join(timeout=5)
        t2.join(timeout=5)

        # Build 1 should succeed
        assert results.get("build1_completed") is True

        # Build 2 should fail with RuntimeError
        assert results.get("build2_failed") is True
        assert "build2" in errors
        assert isinstance(errors["build2"], RuntimeError)
        assert "lock unavailable" in str(errors["build2"]).lower() or project_dir in str(errors["build2"])

    def test_build_error_message_includes_project_path(
        self,
        lock_manager: Any,
    ) -> None:
        """Error message should clearly identify which project is locked."""
        project_dir = "/path/to/my_project"

        with lock_manager.acquire_project_lock(project_dir, blocking=True):
            with pytest.raises(RuntimeError) as exc_info:
                with lock_manager.acquire_project_lock(project_dir, blocking=False):
                    pass

        # Error message should mention the project
        assert project_dir in str(exc_info.value)


class TestDifferentProjectBuilds:
    """Tests for building different projects concurrently."""

    def test_two_builds_different_projects_both_succeed(
        self,
        lock_manager: Any,
    ) -> None:
        """Two concurrent builds of different projects should both succeed."""
        project1 = "/test/project1"
        project2 = "/test/project2"
        results: dict[str, bool] = {}
        start_times: dict[str, float] = {}

        def build_project1() -> None:
            start_times["project1"] = time.time()
            with lock_manager.acquire_project_lock(project1, blocking=True):
                time.sleep(0.3)
                results["project1"] = True

        def build_project2() -> None:
            start_times["project2"] = time.time()
            with lock_manager.acquire_project_lock(project2, blocking=True):
                time.sleep(0.3)
                results["project2"] = True

        t1 = threading.Thread(target=build_project1)
        t2 = threading.Thread(target=build_project2)

        start = time.time()
        t1.start()
        t2.start()
        t1.join(timeout=5)
        t2.join(timeout=5)
        elapsed = time.time() - start

        # Both should succeed
        assert results.get("project1") is True
        assert results.get("project2") is True

        # Total time should be ~0.3s (parallel), not ~0.6s (sequential)
        assert elapsed < 0.5

    def test_multiple_projects_parallel(
        self,
        lock_manager: Any,
    ) -> None:
        """Multiple different projects can build in parallel."""
        projects = [f"/test/project{i}" for i in range(5)]
        results: dict[str, bool] = {}

        def build_project(proj: str) -> None:
            with lock_manager.acquire_project_lock(proj, blocking=True):
                time.sleep(0.1)
                results[proj] = True

        threads = [threading.Thread(target=build_project, args=(p,)) for p in projects]

        start = time.time()
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=5)
        elapsed = time.time() - start

        # All should succeed
        for proj in projects:
            assert results.get(proj) is True

        # Should complete in parallel time, not sequential
        assert elapsed < 0.3  # Much less than 5 * 0.1s = 0.5s


class TestBuildLockReleaseAfterCompletion:
    """Tests for lock release after build completion."""

    def test_build_lock_released_after_completion(
        self,
        lock_manager: Any,
    ) -> None:
        """After first build completes, second build should succeed."""
        project_dir = "/test/project"

        # First build
        with lock_manager.acquire_project_lock(project_dir, blocking=True):
            pass  # Build completes

        # Second build should succeed immediately
        acquired = False
        with lock_manager.acquire_project_lock(project_dir, blocking=False):
            acquired = True

        assert acquired is True

    def test_sequential_builds_same_project_all_succeed(
        self,
        lock_manager: Any,
    ) -> None:
        """Sequential builds of same project should all succeed."""
        project_dir = "/test/project"

        for i in range(5):
            with lock_manager.acquire_project_lock(project_dir, blocking=True):
                pass  # Each build succeeds

        # Lock status should show 5 acquisitions
        status = lock_manager.get_lock_status()
        assert status["project_locks"][project_dir] == 5

    def test_lock_released_on_build_failure(
        self,
        lock_manager: Any,
    ) -> None:
        """Lock should be released even if build raises exception."""
        project_dir = "/test/project"

        # Simulate build failure
        with pytest.raises(RuntimeError):
            with lock_manager.acquire_project_lock(project_dir, blocking=True):
                raise RuntimeError("Build failed!")

        # Lock should be released - next build should succeed
        with lock_manager.acquire_project_lock(project_dir, blocking=False):
            pass


class TestBuildProcessorLocking:
    """Tests for build processor lock behavior using mock context."""

    def test_build_processor_acquires_project_lock(
        self,
        mock_daemon_context: Any,
    ) -> None:
        """BuildRequestProcessor should acquire project lock."""
        from fbuild.daemon.processors.build_processor import BuildRequestProcessor

        processor = BuildRequestProcessor()

        # Create mock request
        mock_request = MagicMock()
        mock_request.project_dir = "/test/project"
        mock_request.environment = "esp32c6"
        mock_request.request_id = "test_123"

        # Get required locks
        locks = processor.get_required_locks(mock_request, mock_daemon_context)

        # Should only require project lock
        assert "project" in locks
        assert locks["project"] == "/test/project"
        assert "port" not in locks

    def test_build_lock_conflict_returns_error_message(
        self,
        lock_manager: Any,
    ) -> None:
        """When lock unavailable, should provide clear error message."""
        project_dir = "/test/project"

        # Hold the lock
        with lock_manager.acquire_project_lock(project_dir, blocking=True):
            # Try to acquire again
            try:
                with lock_manager.acquire_project_lock(project_dir, blocking=False):
                    pass
                pytest.fail("Should have raised RuntimeError")
            except RuntimeError as e:
                # Error should mention the project
                assert project_dir in str(e) or "unavailable" in str(e).lower()


class TestBuildLockWithPortLock:
    """Tests for builds that also need port access (e.g., deploy)."""

    def test_build_with_port_acquires_both_locks(
        self,
        lock_manager: Any,
    ) -> None:
        """Operations needing both project and port should acquire both."""
        project_dir = "/test/project"
        port = "COM3"

        with lock_manager.acquire_project_lock(project_dir, blocking=True):
            with lock_manager.acquire_port_lock(port, blocking=True):
                # Both locks held
                status = lock_manager.get_lock_status()
                assert project_dir in status["project_locks"]
                assert port in status["port_locks"]

    def test_port_lock_does_not_block_different_project_build(
        self,
        lock_manager: Any,
    ) -> None:
        """Port lock should not prevent building different project."""
        port = "COM3"
        project1 = "/test/project1"
        project2 = "/test/project2"

        # Hold port and project1 locks
        with lock_manager.acquire_port_lock(port, blocking=True):
            with lock_manager.acquire_project_lock(project1, blocking=True):
                # Should be able to build project2
                with lock_manager.acquire_project_lock(project2, blocking=False):
                    pass  # Success
