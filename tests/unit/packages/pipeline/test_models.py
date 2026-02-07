"""Unit tests for pipeline data models."""

import time

import pytest

from fbuild.packages.pipeline.models import PackageTask, PipelineResult, TaskPhase


class TestTaskPhase:
    """Tests for TaskPhase enum."""

    def test_all_phases_exist(self):
        """All expected phases are defined."""
        assert TaskPhase.WAITING.value == "waiting"
        assert TaskPhase.DOWNLOADING.value == "downloading"
        assert TaskPhase.UNPACKING.value == "unpacking"
        assert TaskPhase.INSTALLING.value == "installing"
        assert TaskPhase.DONE.value == "done"
        assert TaskPhase.FAILED.value == "failed"

    def test_phase_count(self):
        """Exactly 6 phases are defined."""
        assert len(TaskPhase) == 6

    def test_phase_from_value(self):
        """Phases can be constructed from string values."""
        assert TaskPhase("waiting") == TaskPhase.WAITING
        assert TaskPhase("downloading") == TaskPhase.DOWNLOADING
        assert TaskPhase("done") == TaskPhase.DONE

    def test_invalid_phase_raises(self):
        """Invalid phase value raises ValueError."""
        with pytest.raises(ValueError):
            TaskPhase("invalid_phase")


class TestPackageTask:
    """Tests for PackageTask dataclass."""

    def _make_task(self, **overrides):
        """Create a PackageTask with sensible defaults for testing."""
        defaults = {
            "name": "test-package",
            "url": "https://example.com/test.tar.gz",
            "version": "1.0.0",
            "dest_path": "/tmp/test-package",
        }
        defaults.update(overrides)
        return PackageTask(**defaults)

    def test_basic_creation(self):
        """Tasks can be created with required fields only."""
        task = self._make_task()
        assert task.name == "test-package"
        assert task.url == "https://example.com/test.tar.gz"
        assert task.version == "1.0.0"
        assert task.dest_path == "/tmp/test-package"

    def test_default_values(self):
        """Default values are set correctly."""
        task = self._make_task()
        assert task.dependencies == []
        assert task.phase == TaskPhase.WAITING
        assert task.progress_pct == 0.0
        assert task.status_text == ""
        assert task.elapsed == 0.0
        assert task.error_message == ""
        assert task.archive_path == ""
        assert task.extracted_path == ""
        assert task.start_time is None
        assert task.total_bytes == 0
        assert task.downloaded_bytes == 0

    def test_custom_dependencies(self):
        """Tasks can be created with dependencies."""
        task = self._make_task(dependencies=["dep-a", "dep-b"])
        assert task.dependencies == ["dep-a", "dep-b"]

    def test_mark_started(self):
        """mark_started() records the current time."""
        task = self._make_task()
        assert task.start_time is None
        task.mark_started()
        assert task.start_time is not None
        assert task.start_time > 0

    def test_update_elapsed(self):
        """update_elapsed() calculates time since start."""
        task = self._make_task()
        task.mark_started()
        time.sleep(0.05)
        task.update_elapsed()
        assert task.elapsed >= 0.04  # Allow small timing variance

    def test_update_elapsed_without_start(self):
        """update_elapsed() is a no-op if not started."""
        task = self._make_task()
        task.update_elapsed()
        assert task.elapsed == 0.0

    def test_fail(self):
        """fail() sets phase and error message."""
        task = self._make_task()
        task.mark_started()
        task.fail("Network error")
        assert task.phase == TaskPhase.FAILED
        assert task.error_message == "Network error"

    def test_to_dict(self):
        """to_dict() produces correct dictionary."""
        task = self._make_task(dependencies=["dep-a"])
        task.phase = TaskPhase.DOWNLOADING
        task.progress_pct = 45.0
        task.status_text = "Downloading..."

        d = task.to_dict()
        assert d["name"] == "test-package"
        assert d["url"] == "https://example.com/test.tar.gz"
        assert d["version"] == "1.0.0"
        assert d["dest_path"] == "/tmp/test-package"
        assert d["dependencies"] == ["dep-a"]
        assert d["phase"] == "downloading"
        assert d["progress_pct"] == 45.0
        assert d["status_text"] == "Downloading..."

    def test_from_dict(self):
        """from_dict() reconstructs task correctly."""
        d = {
            "name": "rebuilt-task",
            "url": "https://example.com/rebuilt.tar.gz",
            "version": "2.0.0",
            "dest_path": "/tmp/rebuilt",
            "dependencies": ["parent"],
            "phase": "unpacking",
            "progress_pct": 78.5,
            "status_text": "Extracting...",
            "elapsed": 3.14,
            "error_message": "",
            "archive_path": "/tmp/archive.tar.gz",
            "extracted_path": "/tmp/extracted",
            "total_bytes": 1024,
            "downloaded_bytes": 512,
        }
        task = PackageTask.from_dict(d)
        assert task.name == "rebuilt-task"
        assert task.version == "2.0.0"
        assert task.phase == TaskPhase.UNPACKING
        assert task.progress_pct == 78.5
        assert task.dependencies == ["parent"]
        assert task.total_bytes == 1024
        assert task.downloaded_bytes == 512

    def test_roundtrip_serialization(self):
        """to_dict() -> from_dict() produces equivalent task."""
        original = self._make_task(dependencies=["a", "b"])
        original.phase = TaskPhase.INSTALLING
        original.progress_pct = 99.9
        original.status_text = "Finalizing..."
        original.elapsed = 12.5

        reconstructed = PackageTask.from_dict(original.to_dict())
        assert reconstructed.name == original.name
        assert reconstructed.url == original.url
        assert reconstructed.version == original.version
        assert reconstructed.dest_path == original.dest_path
        assert reconstructed.dependencies == original.dependencies
        assert reconstructed.phase == original.phase
        assert reconstructed.progress_pct == original.progress_pct
        assert reconstructed.status_text == original.status_text
        assert reconstructed.elapsed == original.elapsed

    def test_from_dict_minimal(self):
        """from_dict() works with only required fields."""
        d = {
            "name": "minimal",
            "url": "https://example.com/min.tar.gz",
            "version": "0.1",
            "dest_path": "/tmp/min",
        }
        task = PackageTask.from_dict(d)
        assert task.name == "minimal"
        assert task.phase == TaskPhase.WAITING
        assert task.dependencies == []
        assert task.progress_pct == 0.0

    def test_dependencies_list_is_independent(self):
        """Dependencies list is not shared between instances."""
        task1 = self._make_task()
        task2 = self._make_task()
        task1.dependencies.append("extra")
        assert "extra" not in task2.dependencies


class TestPipelineResult:
    """Tests for PipelineResult dataclass."""

    def _make_task(self, name: str, phase: TaskPhase):
        """Create a task with a specific phase for testing."""
        task = PackageTask(
            name=name,
            url=f"https://example.com/{name}.tar.gz",
            version="1.0",
            dest_path=f"/tmp/{name}",
        )
        task.phase = phase
        return task

    def test_success_result(self):
        """A fully successful pipeline result."""
        tasks = [
            self._make_task("a", TaskPhase.DONE),
            self._make_task("b", TaskPhase.DONE),
        ]
        result = PipelineResult(tasks=tasks, total_elapsed=5.0, success=True)
        assert result.success is True
        assert result.completed_count == 2
        assert result.failed_count == 0
        assert result.failed_tasks == []

    def test_partial_failure(self):
        """A pipeline result with some failures."""
        tasks = [
            self._make_task("a", TaskPhase.DONE),
            self._make_task("b", TaskPhase.FAILED),
            self._make_task("c", TaskPhase.DONE),
        ]
        result = PipelineResult(tasks=tasks, total_elapsed=10.0, success=False)
        assert result.success is False
        assert result.completed_count == 2
        assert result.failed_count == 1
        assert len(result.failed_tasks) == 1
        assert result.failed_tasks[0].name == "b"

    def test_to_dict(self):
        """to_dict() serializes all fields."""
        tasks = [self._make_task("pkg", TaskPhase.DONE)]
        result = PipelineResult(tasks=tasks, total_elapsed=1.5, success=True)
        d = result.to_dict()
        assert d["total_elapsed"] == 1.5
        assert d["success"] is True
        assert len(d["tasks"]) == 1
        assert d["tasks"][0]["name"] == "pkg"
        assert d["tasks"][0]["phase"] == "done"

    def test_from_dict(self):
        """from_dict() reconstructs result correctly."""
        d = {
            "tasks": [
                {
                    "name": "pkg",
                    "url": "https://example.com/pkg.tar.gz",
                    "version": "1.0",
                    "dest_path": "/tmp/pkg",
                    "phase": "done",
                }
            ],
            "total_elapsed": 2.5,
            "success": True,
        }
        result = PipelineResult.from_dict(d)
        assert result.success is True
        assert result.total_elapsed == 2.5
        assert len(result.tasks) == 1
        assert result.tasks[0].name == "pkg"
        assert result.tasks[0].phase == TaskPhase.DONE

    def test_roundtrip_serialization(self):
        """to_dict() -> from_dict() produces equivalent result."""
        tasks = [
            self._make_task("a", TaskPhase.DONE),
            self._make_task("b", TaskPhase.FAILED),
        ]
        tasks[1].error_message = "Download failed"
        original = PipelineResult(tasks=tasks, total_elapsed=7.3, success=False)
        reconstructed = PipelineResult.from_dict(original.to_dict())
        assert reconstructed.success == original.success
        assert reconstructed.total_elapsed == original.total_elapsed
        assert reconstructed.completed_count == original.completed_count
        assert reconstructed.failed_count == original.failed_count

    def test_empty_result(self):
        """An empty pipeline result (no tasks)."""
        result = PipelineResult(tasks=[], total_elapsed=0.0, success=True)
        assert result.completed_count == 0
        assert result.failed_count == 0
        assert result.failed_tasks == []
