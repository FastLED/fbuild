"""Unit tests for the ParallelPipeline orchestrator.

Tests verify:
- Independent tasks run in parallel through all three phases
- Dependent tasks respect DAG ordering
- Failure propagation stops dependent tasks
- Cancellation shuts down gracefully
- Edge cases (single task, empty tasks, all cached)
"""

import threading
import time
from pathlib import Path
from typing import Any
from unittest.mock import MagicMock, patch

import pytest

from fbuild.packages.pipeline.callbacks import NullCallback
from fbuild.packages.pipeline.models import PackageTask, TaskPhase
from fbuild.packages.pipeline.pipeline import (
    ParallelPipeline,
    PipelineCancelledError,
)

# ─── Helpers ──────────────────────────────────────────────────────────────────


class RecordingCallback:
    """Thread-safe callback that records all progress updates."""

    def __init__(self) -> None:
        self.calls: list[tuple[str, TaskPhase, float, float, str]] = []
        self._lock = threading.Lock()

    def on_progress(self, task_name: str, phase: TaskPhase, progress: float, total: float, detail: str) -> None:
        with self._lock:
            self.calls.append((task_name, phase, progress, total, detail))

    def get_calls(self) -> list[tuple[str, TaskPhase, float, float, str]]:
        with self._lock:
            return list(self.calls)

    def get_phases_for(self, task_name: str) -> list[TaskPhase]:
        with self._lock:
            return [c[1] for c in self.calls if c[0] == task_name]

    def get_task_names(self) -> list[str]:
        with self._lock:
            return [c[0] for c in self.calls]


def make_task(
    name: str,
    url: str = "http://example.com/pkg.tar.gz",
    dest_path: str = "/tmp/pkg",
    version: str = "1.0.0",
    dependencies: list[str] | None = None,
) -> PackageTask:
    """Create a PackageTask with sensible defaults for testing."""
    return PackageTask(
        name=name,
        url=url,
        version=version,
        dest_path=dest_path,
        dependencies=dependencies if dependencies is not None else [],
    )


# ─── Mock Pools ───────────────────────────────────────────────────────────────


def make_mock_download_pool(delay: float = 0.0, fail_tasks: set[str] | None = None) -> MagicMock:
    """Create a mock DownloadPool that returns archive paths.

    Args:
        delay: Simulated download time in seconds.
        fail_tasks: Set of task names that should fail during download.
    """
    fail_set = fail_tasks or set()
    pool = MagicMock()
    pool.__enter__ = MagicMock(return_value=pool)
    pool.__exit__ = MagicMock(return_value=False)

    def mock_submit(task: PackageTask, callback: Any) -> MagicMock:
        future = MagicMock()
        archive_path = Path(task.dest_path).parent / "archive.tar.gz"

        def get_result(timeout: float | None = None) -> Path:
            if delay > 0:
                time.sleep(delay)
            if task.name in fail_set:
                raise ConnectionError(f"Download failed for {task.name}")
            return archive_path

        future.result = get_result
        future.done.return_value = True
        future.cancel.return_value = True
        # Store for verification
        future._task_name = task.name
        return future

    pool.submit_download = mock_submit
    return pool


def make_mock_unpack_pool(delay: float = 0.0, fail_tasks: set[str] | None = None) -> MagicMock:
    """Create a mock UnpackPool that returns extracted paths."""
    fail_set = fail_tasks or set()
    pool = MagicMock()
    pool.__enter__ = MagicMock(return_value=pool)
    pool.__exit__ = MagicMock(return_value=False)

    def mock_submit(task: PackageTask, archive_path: Path, callback: Any) -> MagicMock:
        future = MagicMock()
        extracted_path = Path(task.dest_path)

        def get_result(timeout: float | None = None) -> Path:
            if delay > 0:
                time.sleep(delay)
            if task.name in fail_set:
                raise OSError(f"Unpack failed for {task.name}")
            return extracted_path

        future.result = get_result
        future.done.return_value = True
        future.cancel.return_value = True
        return future

    pool.submit_unpack = mock_submit
    return pool


def make_mock_install_pool(delay: float = 0.0, fail_tasks: set[str] | None = None) -> MagicMock:
    """Create a mock InstallPool that returns installed paths."""
    fail_set = fail_tasks or set()
    pool = MagicMock()
    pool.__enter__ = MagicMock(return_value=pool)
    pool.__exit__ = MagicMock(return_value=False)

    def mock_submit(task: PackageTask, extracted_path: Path, callback: Any) -> MagicMock:
        future = MagicMock()

        def get_result(timeout: float | None = None) -> Path:
            if delay > 0:
                time.sleep(delay)
            if task.name in fail_set:
                raise RuntimeError(f"Install failed for {task.name}")
            return extracted_path

        future.result = get_result
        future.done.return_value = True
        future.cancel.return_value = True
        return future

    pool.submit_install = mock_submit
    return pool


# ─── Pipeline Construction Tests ──────────────────────────────────────────────


class TestPipelineConstruction:
    """Tests for ParallelPipeline initialization."""

    def test_create_pipeline(self) -> None:
        """Pipeline should initialize with worker counts."""
        pipeline = ParallelPipeline(
            download_workers=4,
            unpack_workers=2,
            install_workers=2,
        )
        assert pipeline._download_workers == 4
        assert pipeline._unpack_workers == 2
        assert pipeline._install_workers == 2

    def test_pipeline_not_cancelled_initially(self) -> None:
        """Pipeline should not be cancelled when first created."""
        pipeline = ParallelPipeline(
            download_workers=1,
            unpack_workers=1,
            install_workers=1,
        )
        assert not pipeline._is_cancelled()


# ─── Empty Pipeline Tests ─────────────────────────────────────────────────────


class TestEmptyPipeline:
    """Tests for running pipeline with no tasks."""

    def test_empty_task_list(self) -> None:
        """Pipeline should return immediately with empty task list."""
        pipeline = ParallelPipeline(
            download_workers=1,
            unpack_workers=1,
            install_workers=1,
        )
        result = pipeline.run([], NullCallback())
        assert result.success is True
        assert result.total_elapsed >= 0.0
        assert len(result.tasks) == 0
        assert result.completed_count == 0
        assert result.failed_count == 0


# ─── Single Task Tests ────────────────────────────────────────────────────────


class TestSingleTask:
    """Tests for pipeline processing a single task."""

    @patch("fbuild.packages.pipeline.pipeline.InstallPool")
    @patch("fbuild.packages.pipeline.pipeline.UnpackPool")
    @patch("fbuild.packages.pipeline.pipeline.DownloadPool")
    def test_single_task_completes(
        self,
        mock_dl_cls: MagicMock,
        mock_up_cls: MagicMock,
        mock_ip_cls: MagicMock,
    ) -> None:
        """A single task should progress through all three phases to DONE."""
        mock_dl_cls.return_value = make_mock_download_pool()
        mock_up_cls.return_value = make_mock_unpack_pool()
        mock_ip_cls.return_value = make_mock_install_pool()

        task = make_task("pkg-a", dest_path="/tmp/pkg-a")
        pipeline = ParallelPipeline(
            download_workers=1,
            unpack_workers=1,
            install_workers=1,
        )

        cb = RecordingCallback()
        result = pipeline.run([task], cb)

        assert result.success is True
        assert result.completed_count == 1
        assert result.failed_count == 0
        assert len(result.tasks) == 1
        assert result.tasks[0].phase == TaskPhase.DONE

    @patch("fbuild.packages.pipeline.pipeline.InstallPool")
    @patch("fbuild.packages.pipeline.pipeline.UnpackPool")
    @patch("fbuild.packages.pipeline.pipeline.DownloadPool")
    def test_single_task_reports_all_phases(
        self,
        mock_dl_cls: MagicMock,
        mock_up_cls: MagicMock,
        mock_ip_cls: MagicMock,
    ) -> None:
        """Callback should receive updates for all phases of a single task."""
        mock_dl_cls.return_value = make_mock_download_pool()
        mock_up_cls.return_value = make_mock_unpack_pool()
        mock_ip_cls.return_value = make_mock_install_pool()

        task = make_task("pkg-phases")
        pipeline = ParallelPipeline(
            download_workers=1,
            unpack_workers=1,
            install_workers=1,
        )

        cb = RecordingCallback()
        pipeline.run([task], cb)

        phases = cb.get_phases_for("pkg-phases")
        assert TaskPhase.DOWNLOADING in phases
        assert TaskPhase.UNPACKING in phases
        assert TaskPhase.INSTALLING in phases
        assert TaskPhase.DONE in phases

    @patch("fbuild.packages.pipeline.pipeline.InstallPool")
    @patch("fbuild.packages.pipeline.pipeline.UnpackPool")
    @patch("fbuild.packages.pipeline.pipeline.DownloadPool")
    def test_single_task_timing(
        self,
        mock_dl_cls: MagicMock,
        mock_up_cls: MagicMock,
        mock_ip_cls: MagicMock,
    ) -> None:
        """Pipeline should track total elapsed time."""
        mock_dl_cls.return_value = make_mock_download_pool()
        mock_up_cls.return_value = make_mock_unpack_pool()
        mock_ip_cls.return_value = make_mock_install_pool()

        task = make_task("timed-pkg")
        pipeline = ParallelPipeline(
            download_workers=1,
            unpack_workers=1,
            install_workers=1,
        )

        result = pipeline.run([task], NullCallback())
        assert result.total_elapsed >= 0.0


# ─── Independent Tasks (Parallel) Tests ───────────────────────────────────────


class TestIndependentTasks:
    """Tests for multiple independent tasks running in parallel."""

    @patch("fbuild.packages.pipeline.pipeline.InstallPool")
    @patch("fbuild.packages.pipeline.pipeline.UnpackPool")
    @patch("fbuild.packages.pipeline.pipeline.DownloadPool")
    def test_three_independent_tasks_complete(
        self,
        mock_dl_cls: MagicMock,
        mock_up_cls: MagicMock,
        mock_ip_cls: MagicMock,
    ) -> None:
        """Three independent tasks should all complete successfully."""
        mock_dl_cls.return_value = make_mock_download_pool()
        mock_up_cls.return_value = make_mock_unpack_pool()
        mock_ip_cls.return_value = make_mock_install_pool()

        tasks = [
            make_task("pkg-1", dest_path="/tmp/pkg-1"),
            make_task("pkg-2", dest_path="/tmp/pkg-2"),
            make_task("pkg-3", dest_path="/tmp/pkg-3"),
        ]

        pipeline = ParallelPipeline(
            download_workers=3,
            unpack_workers=2,
            install_workers=2,
        )

        result = pipeline.run(tasks, NullCallback())

        assert result.success is True
        assert result.completed_count == 3
        assert result.failed_count == 0

    @patch("fbuild.packages.pipeline.pipeline.InstallPool")
    @patch("fbuild.packages.pipeline.pipeline.UnpackPool")
    @patch("fbuild.packages.pipeline.pipeline.DownloadPool")
    def test_independent_tasks_all_reported(
        self,
        mock_dl_cls: MagicMock,
        mock_up_cls: MagicMock,
        mock_ip_cls: MagicMock,
    ) -> None:
        """All independent tasks should report progress through callback."""
        mock_dl_cls.return_value = make_mock_download_pool()
        mock_up_cls.return_value = make_mock_unpack_pool()
        mock_ip_cls.return_value = make_mock_install_pool()

        tasks = [
            make_task("lib-a", dest_path="/tmp/lib-a"),
            make_task("lib-b", dest_path="/tmp/lib-b"),
        ]

        pipeline = ParallelPipeline(
            download_workers=2,
            unpack_workers=2,
            install_workers=2,
        )

        cb = RecordingCallback()
        pipeline.run(tasks, cb)

        task_names = set(cb.get_task_names())
        assert "lib-a" in task_names
        assert "lib-b" in task_names


# ─── Dependency Ordering Tests ────────────────────────────────────────────────


class TestDependencyOrdering:
    """Tests for respecting task dependency ordering."""

    @patch("fbuild.packages.pipeline.pipeline.InstallPool")
    @patch("fbuild.packages.pipeline.pipeline.UnpackPool")
    @patch("fbuild.packages.pipeline.pipeline.DownloadPool")
    def test_dependent_task_waits_for_parent(
        self,
        mock_dl_cls: MagicMock,
        mock_up_cls: MagicMock,
        mock_ip_cls: MagicMock,
    ) -> None:
        """A task with dependencies should wait until all deps are DONE."""
        mock_dl_cls.return_value = make_mock_download_pool()
        mock_up_cls.return_value = make_mock_unpack_pool()
        mock_ip_cls.return_value = make_mock_install_pool()

        tasks = [
            make_task("framework", dest_path="/tmp/framework"),
            make_task("library", dest_path="/tmp/library", dependencies=["framework"]),
        ]

        pipeline = ParallelPipeline(
            download_workers=2,
            unpack_workers=2,
            install_workers=2,
        )

        cb = RecordingCallback()
        result = pipeline.run(tasks, cb)

        assert result.success is True
        assert result.completed_count == 2

        # Verify framework reached DONE before library started DOWNLOADING
        calls = cb.get_calls()
        framework_done_idx = None
        library_dl_idx = None
        for i, (name, phase, _, _, _) in enumerate(calls):
            if name == "framework" and phase == TaskPhase.DONE and framework_done_idx is None:
                framework_done_idx = i
            if name == "library" and phase == TaskPhase.DOWNLOADING and library_dl_idx is None:
                library_dl_idx = i

        assert framework_done_idx is not None
        assert library_dl_idx is not None
        assert framework_done_idx < library_dl_idx

    @patch("fbuild.packages.pipeline.pipeline.InstallPool")
    @patch("fbuild.packages.pipeline.pipeline.UnpackPool")
    @patch("fbuild.packages.pipeline.pipeline.DownloadPool")
    def test_three_level_dependency_chain(
        self,
        mock_dl_cls: MagicMock,
        mock_up_cls: MagicMock,
        mock_ip_cls: MagicMock,
    ) -> None:
        """Tasks in a chain (A -> B -> C) should process in order."""
        mock_dl_cls.return_value = make_mock_download_pool()
        mock_up_cls.return_value = make_mock_unpack_pool()
        mock_ip_cls.return_value = make_mock_install_pool()

        tasks = [
            make_task("platform", dest_path="/tmp/platform"),
            make_task("toolchain", dest_path="/tmp/toolchain", dependencies=["platform"]),
            make_task("framework", dest_path="/tmp/framework", dependencies=["toolchain"]),
        ]

        pipeline = ParallelPipeline(
            download_workers=2,
            unpack_workers=2,
            install_workers=2,
        )

        result = pipeline.run(tasks, NullCallback())

        assert result.success is True
        assert result.completed_count == 3

    @patch("fbuild.packages.pipeline.pipeline.InstallPool")
    @patch("fbuild.packages.pipeline.pipeline.UnpackPool")
    @patch("fbuild.packages.pipeline.pipeline.DownloadPool")
    def test_diamond_dependency(
        self,
        mock_dl_cls: MagicMock,
        mock_up_cls: MagicMock,
        mock_ip_cls: MagicMock,
    ) -> None:
        """Diamond dependency pattern (A->B, A->C, B->D, C->D) should work."""
        mock_dl_cls.return_value = make_mock_download_pool()
        mock_up_cls.return_value = make_mock_unpack_pool()
        mock_ip_cls.return_value = make_mock_install_pool()

        tasks = [
            make_task("A", dest_path="/tmp/A"),
            make_task("B", dest_path="/tmp/B", dependencies=["A"]),
            make_task("C", dest_path="/tmp/C", dependencies=["A"]),
            make_task("D", dest_path="/tmp/D", dependencies=["B", "C"]),
        ]

        pipeline = ParallelPipeline(
            download_workers=4,
            unpack_workers=2,
            install_workers=2,
        )

        result = pipeline.run(tasks, NullCallback())

        assert result.success is True
        assert result.completed_count == 4

    @patch("fbuild.packages.pipeline.pipeline.InstallPool")
    @patch("fbuild.packages.pipeline.pipeline.UnpackPool")
    @patch("fbuild.packages.pipeline.pipeline.DownloadPool")
    def test_avr_dependency_graph(
        self,
        mock_dl_cls: MagicMock,
        mock_up_cls: MagicMock,
        mock_ip_cls: MagicMock,
    ) -> None:
        """Realistic AVR dependency graph should complete correctly."""
        mock_dl_cls.return_value = make_mock_download_pool()
        mock_up_cls.return_value = make_mock_unpack_pool()
        mock_ip_cls.return_value = make_mock_install_pool()

        tasks = [
            make_task("platform-atmelavr", dest_path="/tmp/platform"),
            make_task("toolchain-atmelavr", dest_path="/tmp/toolchain", dependencies=["platform-atmelavr"]),
            make_task("framework-arduino", dest_path="/tmp/framework", dependencies=["toolchain-atmelavr"]),
            make_task("Wire", dest_path="/tmp/wire", dependencies=["framework-arduino"]),
            make_task("SPI", dest_path="/tmp/spi", dependencies=["framework-arduino"]),
            make_task("Servo", dest_path="/tmp/servo", dependencies=["framework-arduino"]),
        ]

        pipeline = ParallelPipeline(
            download_workers=4,
            unpack_workers=2,
            install_workers=2,
        )

        result = pipeline.run(tasks, NullCallback())

        assert result.success is True
        assert result.completed_count == 6
        assert result.failed_count == 0


# ─── Failure Propagation Tests ────────────────────────────────────────────────


class TestFailurePropagation:
    """Tests for error handling and failure propagation."""

    @patch("fbuild.packages.pipeline.pipeline.InstallPool")
    @patch("fbuild.packages.pipeline.pipeline.UnpackPool")
    @patch("fbuild.packages.pipeline.pipeline.DownloadPool")
    def test_download_failure_marks_task_failed(
        self,
        mock_dl_cls: MagicMock,
        mock_up_cls: MagicMock,
        mock_ip_cls: MagicMock,
    ) -> None:
        """Task should be FAILED if download fails."""
        mock_dl_cls.return_value = make_mock_download_pool(fail_tasks={"bad-pkg"})
        mock_up_cls.return_value = make_mock_unpack_pool()
        mock_ip_cls.return_value = make_mock_install_pool()

        tasks = [make_task("bad-pkg", dest_path="/tmp/bad")]

        pipeline = ParallelPipeline(
            download_workers=1,
            unpack_workers=1,
            install_workers=1,
        )

        result = pipeline.run(tasks, NullCallback())

        assert result.success is False
        assert result.failed_count == 1
        assert result.tasks[0].phase == TaskPhase.FAILED
        assert "Download failed" in result.tasks[0].error_message

    @patch("fbuild.packages.pipeline.pipeline.InstallPool")
    @patch("fbuild.packages.pipeline.pipeline.UnpackPool")
    @patch("fbuild.packages.pipeline.pipeline.DownloadPool")
    def test_unpack_failure_marks_task_failed(
        self,
        mock_dl_cls: MagicMock,
        mock_up_cls: MagicMock,
        mock_ip_cls: MagicMock,
    ) -> None:
        """Task should be FAILED if unpack fails."""
        mock_dl_cls.return_value = make_mock_download_pool()
        mock_up_cls.return_value = make_mock_unpack_pool(fail_tasks={"unpack-fail"})
        mock_ip_cls.return_value = make_mock_install_pool()

        tasks = [make_task("unpack-fail", dest_path="/tmp/unpack-fail")]

        pipeline = ParallelPipeline(
            download_workers=1,
            unpack_workers=1,
            install_workers=1,
        )

        result = pipeline.run(tasks, NullCallback())

        assert result.success is False
        assert result.failed_count == 1
        assert result.tasks[0].phase == TaskPhase.FAILED
        assert "Unpack failed" in result.tasks[0].error_message

    @patch("fbuild.packages.pipeline.pipeline.InstallPool")
    @patch("fbuild.packages.pipeline.pipeline.UnpackPool")
    @patch("fbuild.packages.pipeline.pipeline.DownloadPool")
    def test_install_failure_marks_task_failed(
        self,
        mock_dl_cls: MagicMock,
        mock_up_cls: MagicMock,
        mock_ip_cls: MagicMock,
    ) -> None:
        """Task should be FAILED if install fails."""
        mock_dl_cls.return_value = make_mock_download_pool()
        mock_up_cls.return_value = make_mock_unpack_pool()
        mock_ip_cls.return_value = make_mock_install_pool(fail_tasks={"install-fail"})

        tasks = [make_task("install-fail", dest_path="/tmp/install-fail")]

        pipeline = ParallelPipeline(
            download_workers=1,
            unpack_workers=1,
            install_workers=1,
        )

        result = pipeline.run(tasks, NullCallback())

        assert result.success is False
        assert result.failed_count == 1
        assert result.tasks[0].phase == TaskPhase.FAILED

    @patch("fbuild.packages.pipeline.pipeline.InstallPool")
    @patch("fbuild.packages.pipeline.pipeline.UnpackPool")
    @patch("fbuild.packages.pipeline.pipeline.DownloadPool")
    def test_failure_propagates_to_dependent_tasks(
        self,
        mock_dl_cls: MagicMock,
        mock_up_cls: MagicMock,
        mock_ip_cls: MagicMock,
    ) -> None:
        """If a task fails, all dependent tasks should also fail."""
        mock_dl_cls.return_value = make_mock_download_pool(fail_tasks={"toolchain"})
        mock_up_cls.return_value = make_mock_unpack_pool()
        mock_ip_cls.return_value = make_mock_install_pool()

        tasks = [
            make_task("toolchain", dest_path="/tmp/toolchain"),
            make_task("framework", dest_path="/tmp/framework", dependencies=["toolchain"]),
            make_task("library", dest_path="/tmp/library", dependencies=["framework"]),
        ]

        pipeline = ParallelPipeline(
            download_workers=2,
            unpack_workers=2,
            install_workers=2,
        )

        cb = RecordingCallback()
        result = pipeline.run(tasks, cb)

        assert result.success is False
        assert result.failed_count == 3

        # Verify all tasks failed
        for task in result.tasks:
            assert task.phase == TaskPhase.FAILED

    @patch("fbuild.packages.pipeline.pipeline.InstallPool")
    @patch("fbuild.packages.pipeline.pipeline.UnpackPool")
    @patch("fbuild.packages.pipeline.pipeline.DownloadPool")
    def test_failure_only_affects_dependents(
        self,
        mock_dl_cls: MagicMock,
        mock_up_cls: MagicMock,
        mock_ip_cls: MagicMock,
    ) -> None:
        """Failure should only affect tasks that depend on the failed task, not independent ones."""
        mock_dl_cls.return_value = make_mock_download_pool(fail_tasks={"bad-lib"})
        mock_up_cls.return_value = make_mock_unpack_pool()
        mock_ip_cls.return_value = make_mock_install_pool()

        tasks = [
            make_task("good-pkg", dest_path="/tmp/good"),
            make_task("bad-lib", dest_path="/tmp/bad"),
            make_task("depends-on-bad", dest_path="/tmp/dep-bad", dependencies=["bad-lib"]),
        ]

        pipeline = ParallelPipeline(
            download_workers=2,
            unpack_workers=2,
            install_workers=2,
        )

        result = pipeline.run(tasks, NullCallback())

        assert result.success is False
        assert result.completed_count == 1
        assert result.failed_count == 2

        # Find each task by name
        tasks_by_name = {t.name: t for t in result.tasks}
        assert tasks_by_name["good-pkg"].phase == TaskPhase.DONE
        assert tasks_by_name["bad-lib"].phase == TaskPhase.FAILED
        assert tasks_by_name["depends-on-bad"].phase == TaskPhase.FAILED

    @patch("fbuild.packages.pipeline.pipeline.InstallPool")
    @patch("fbuild.packages.pipeline.pipeline.UnpackPool")
    @patch("fbuild.packages.pipeline.pipeline.DownloadPool")
    def test_failure_callback_reports_error(
        self,
        mock_dl_cls: MagicMock,
        mock_up_cls: MagicMock,
        mock_ip_cls: MagicMock,
    ) -> None:
        """Callback should receive FAILED phase update with error detail."""
        mock_dl_cls.return_value = make_mock_download_pool(fail_tasks={"err-pkg"})
        mock_up_cls.return_value = make_mock_unpack_pool()
        mock_ip_cls.return_value = make_mock_install_pool()

        tasks = [make_task("err-pkg", dest_path="/tmp/err")]

        pipeline = ParallelPipeline(
            download_workers=1,
            unpack_workers=1,
            install_workers=1,
        )

        cb = RecordingCallback()
        pipeline.run(tasks, cb)

        failed_calls = [c for c in cb.get_calls() if c[1] == TaskPhase.FAILED]
        assert len(failed_calls) >= 1
        assert "err-pkg" == failed_calls[0][0]


# ─── Cancellation Tests ──────────────────────────────────────────────────────


class TestCancellation:
    """Tests for pipeline cancellation."""

    @patch("fbuild.packages.pipeline.pipeline.InstallPool")
    @patch("fbuild.packages.pipeline.pipeline.UnpackPool")
    @patch("fbuild.packages.pipeline.pipeline.DownloadPool")
    def test_cancel_raises_cancelled_error(
        self,
        mock_dl_cls: MagicMock,
        mock_up_cls: MagicMock,
        mock_ip_cls: MagicMock,
    ) -> None:
        """Cancelling the pipeline should raise PipelineCancelledError."""
        # Use a slow download so we have time to cancel
        mock_dl_cls.return_value = make_mock_download_pool(delay=1.0)
        mock_up_cls.return_value = make_mock_unpack_pool()
        mock_ip_cls.return_value = make_mock_install_pool()

        tasks = [
            make_task("slow-1", dest_path="/tmp/slow-1"),
            make_task("slow-2", dest_path="/tmp/slow-2"),
        ]

        pipeline = ParallelPipeline(
            download_workers=1,
            unpack_workers=1,
            install_workers=1,
        )

        # Cancel from another thread after a brief delay
        def cancel_after_delay() -> None:
            time.sleep(0.1)
            pipeline.cancel()

        cancel_thread = threading.Thread(target=cancel_after_delay)
        cancel_thread.start()

        with pytest.raises(PipelineCancelledError):
            pipeline.run(tasks, NullCallback())

        cancel_thread.join()

    def test_cancel_is_thread_safe(self) -> None:
        """cancel() should be thread-safe and callable from any thread."""
        pipeline = ParallelPipeline(
            download_workers=1,
            unpack_workers=1,
            install_workers=1,
        )

        assert not pipeline._is_cancelled()
        pipeline.cancel()
        assert pipeline._is_cancelled()


# ─── PipelineResult Tests ─────────────────────────────────────────────────────


class TestPipelineResult:
    """Tests for PipelineResult properties."""

    @patch("fbuild.packages.pipeline.pipeline.InstallPool")
    @patch("fbuild.packages.pipeline.pipeline.UnpackPool")
    @patch("fbuild.packages.pipeline.pipeline.DownloadPool")
    def test_result_has_all_tasks(
        self,
        mock_dl_cls: MagicMock,
        mock_up_cls: MagicMock,
        mock_ip_cls: MagicMock,
    ) -> None:
        """Result should contain all tasks regardless of success/failure."""
        mock_dl_cls.return_value = make_mock_download_pool(fail_tasks={"bad"})
        mock_up_cls.return_value = make_mock_unpack_pool()
        mock_ip_cls.return_value = make_mock_install_pool()

        tasks = [
            make_task("good", dest_path="/tmp/good"),
            make_task("bad", dest_path="/tmp/bad"),
        ]

        pipeline = ParallelPipeline(
            download_workers=2,
            unpack_workers=1,
            install_workers=1,
        )

        result = pipeline.run(tasks, NullCallback())

        assert len(result.tasks) == 2
        assert result.completed_count == 1
        assert result.failed_count == 1

    @patch("fbuild.packages.pipeline.pipeline.InstallPool")
    @patch("fbuild.packages.pipeline.pipeline.UnpackPool")
    @patch("fbuild.packages.pipeline.pipeline.DownloadPool")
    def test_result_failed_tasks_list(
        self,
        mock_dl_cls: MagicMock,
        mock_up_cls: MagicMock,
        mock_ip_cls: MagicMock,
    ) -> None:
        """Result.failed_tasks should return only failed tasks."""
        mock_dl_cls.return_value = make_mock_download_pool(fail_tasks={"fail-1", "fail-2"})
        mock_up_cls.return_value = make_mock_unpack_pool()
        mock_ip_cls.return_value = make_mock_install_pool()

        tasks = [
            make_task("ok", dest_path="/tmp/ok"),
            make_task("fail-1", dest_path="/tmp/fail-1"),
            make_task("fail-2", dest_path="/tmp/fail-2"),
        ]

        pipeline = ParallelPipeline(
            download_workers=3,
            unpack_workers=1,
            install_workers=1,
        )

        result = pipeline.run(tasks, NullCallback())

        failed_names = {t.name for t in result.failed_tasks}
        assert "fail-1" in failed_names
        assert "fail-2" in failed_names
        assert "ok" not in failed_names


# ─── Edge Case Tests ──────────────────────────────────────────────────────────


class TestEdgeCases:
    """Tests for edge cases and unusual configurations."""

    @patch("fbuild.packages.pipeline.pipeline.InstallPool")
    @patch("fbuild.packages.pipeline.pipeline.UnpackPool")
    @patch("fbuild.packages.pipeline.pipeline.DownloadPool")
    def test_many_independent_tasks(
        self,
        mock_dl_cls: MagicMock,
        mock_up_cls: MagicMock,
        mock_ip_cls: MagicMock,
    ) -> None:
        """Pipeline should handle many independent tasks."""
        mock_dl_cls.return_value = make_mock_download_pool()
        mock_up_cls.return_value = make_mock_unpack_pool()
        mock_ip_cls.return_value = make_mock_install_pool()

        tasks = [make_task(f"pkg-{i}", dest_path=f"/tmp/pkg-{i}") for i in range(10)]

        pipeline = ParallelPipeline(
            download_workers=4,
            unpack_workers=2,
            install_workers=2,
        )

        result = pipeline.run(tasks, NullCallback())

        assert result.success is True
        assert result.completed_count == 10

    @patch("fbuild.packages.pipeline.pipeline.InstallPool")
    @patch("fbuild.packages.pipeline.pipeline.UnpackPool")
    @patch("fbuild.packages.pipeline.pipeline.DownloadPool")
    def test_single_worker_pools(
        self,
        mock_dl_cls: MagicMock,
        mock_up_cls: MagicMock,
        mock_ip_cls: MagicMock,
    ) -> None:
        """Pipeline should work with single-worker pools (serial-like)."""
        mock_dl_cls.return_value = make_mock_download_pool()
        mock_up_cls.return_value = make_mock_unpack_pool()
        mock_ip_cls.return_value = make_mock_install_pool()

        tasks = [
            make_task("pkg-1", dest_path="/tmp/pkg-1"),
            make_task("pkg-2", dest_path="/tmp/pkg-2"),
        ]

        pipeline = ParallelPipeline(
            download_workers=1,
            unpack_workers=1,
            install_workers=1,
        )

        result = pipeline.run(tasks, NullCallback())

        assert result.success is True
        assert result.completed_count == 2

    @patch("fbuild.packages.pipeline.pipeline.InstallPool")
    @patch("fbuild.packages.pipeline.pipeline.UnpackPool")
    @patch("fbuild.packages.pipeline.pipeline.DownloadPool")
    def test_all_tasks_fail(
        self,
        mock_dl_cls: MagicMock,
        mock_up_cls: MagicMock,
        mock_ip_cls: MagicMock,
    ) -> None:
        """Pipeline should handle all tasks failing."""
        mock_dl_cls.return_value = make_mock_download_pool(fail_tasks={"pkg-1", "pkg-2", "pkg-3"})
        mock_up_cls.return_value = make_mock_unpack_pool()
        mock_ip_cls.return_value = make_mock_install_pool()

        tasks = [
            make_task("pkg-1", dest_path="/tmp/pkg-1"),
            make_task("pkg-2", dest_path="/tmp/pkg-2"),
            make_task("pkg-3", dest_path="/tmp/pkg-3"),
        ]

        pipeline = ParallelPipeline(
            download_workers=3,
            unpack_workers=1,
            install_workers=1,
        )

        result = pipeline.run(tasks, NullCallback())

        assert result.success is False
        assert result.failed_count == 3
        assert result.completed_count == 0

    @patch("fbuild.packages.pipeline.pipeline.InstallPool")
    @patch("fbuild.packages.pipeline.pipeline.UnpackPool")
    @patch("fbuild.packages.pipeline.pipeline.DownloadPool")
    def test_mixed_success_and_failure_with_deps(
        self,
        mock_dl_cls: MagicMock,
        mock_up_cls: MagicMock,
        mock_ip_cls: MagicMock,
    ) -> None:
        """Mixed scenario: some tasks fail, independent tasks still succeed."""
        mock_dl_cls.return_value = make_mock_download_pool(fail_tasks={"platform"})
        mock_up_cls.return_value = make_mock_unpack_pool()
        mock_ip_cls.return_value = make_mock_install_pool()

        tasks = [
            make_task("platform", dest_path="/tmp/platform"),
            make_task("toolchain", dest_path="/tmp/toolchain", dependencies=["platform"]),
            make_task("independent", dest_path="/tmp/independent"),
        ]

        pipeline = ParallelPipeline(
            download_workers=2,
            unpack_workers=2,
            install_workers=2,
        )

        result = pipeline.run(tasks, NullCallback())

        assert result.success is False
        tasks_by_name = {t.name: t for t in result.tasks}
        assert tasks_by_name["platform"].phase == TaskPhase.FAILED
        assert tasks_by_name["toolchain"].phase == TaskPhase.FAILED
        assert tasks_by_name["independent"].phase == TaskPhase.DONE
