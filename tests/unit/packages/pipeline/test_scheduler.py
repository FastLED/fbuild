"""Unit tests for the dependency scheduler."""

import threading

import pytest

from fbuild.packages.pipeline.models import PackageTask, TaskPhase
from fbuild.packages.pipeline.scheduler import (
    CyclicDependencyError,
    DependencyScheduler,
)


def _make_task(name: str, dependencies: list[str] | None = None) -> PackageTask:
    """Create a PackageTask with sensible defaults for testing."""
    return PackageTask(
        name=name,
        url=f"https://example.com/{name}.tar.gz",
        version="1.0.0",
        dest_path=f"/tmp/{name}",
        dependencies=dependencies if dependencies is not None else [],
    )


class TestDependencySchedulerBasic:
    """Basic add/get/validate operations."""

    def test_add_single_task(self):
        """A single task can be added."""
        scheduler = DependencyScheduler()
        scheduler.add_task(_make_task("pkg-a"))
        assert scheduler.task_count == 1

    def test_add_multiple_tasks(self):
        """Multiple tasks can be added."""
        scheduler = DependencyScheduler()
        scheduler.add_task(_make_task("pkg-a"))
        scheduler.add_task(_make_task("pkg-b"))
        scheduler.add_task(_make_task("pkg-c"))
        assert scheduler.task_count == 3

    def test_duplicate_task_raises(self):
        """Adding a task with a duplicate name raises ValueError."""
        scheduler = DependencyScheduler()
        scheduler.add_task(_make_task("pkg-a"))
        with pytest.raises(ValueError, match="Duplicate task name"):
            scheduler.add_task(_make_task("pkg-a"))

    def test_get_task(self):
        """Tasks can be retrieved by name."""
        scheduler = DependencyScheduler()
        task = _make_task("pkg-a")
        scheduler.add_task(task)
        retrieved = scheduler.get_task("pkg-a")
        assert retrieved.name == "pkg-a"

    def test_get_unknown_task_raises(self):
        """Getting a non-existent task raises KeyError."""
        scheduler = DependencyScheduler()
        with pytest.raises(KeyError, match="Unknown task"):
            scheduler.get_task("nonexistent")

    def test_get_all_tasks(self):
        """get_all_tasks returns all registered tasks."""
        scheduler = DependencyScheduler()
        scheduler.add_task(_make_task("a"))
        scheduler.add_task(_make_task("b"))
        all_tasks = scheduler.get_all_tasks()
        names = {t.name for t in all_tasks}
        assert names == {"a", "b"}


class TestDependencySchedulerValidation:
    """Validation: reference checking and cycle detection."""

    def test_validate_valid_graph(self):
        """A valid DAG passes validation."""
        scheduler = DependencyScheduler()
        scheduler.add_task(_make_task("platform"))
        scheduler.add_task(_make_task("toolchain", dependencies=["platform"]))
        scheduler.add_task(_make_task("framework", dependencies=["toolchain"]))
        scheduler.validate()  # Should not raise

    def test_validate_no_deps(self):
        """Independent tasks (no deps) pass validation."""
        scheduler = DependencyScheduler()
        scheduler.add_task(_make_task("a"))
        scheduler.add_task(_make_task("b"))
        scheduler.add_task(_make_task("c"))
        scheduler.validate()  # Should not raise

    def test_validate_missing_dependency(self):
        """Reference to non-existent dependency raises ValueError."""
        scheduler = DependencyScheduler()
        scheduler.add_task(_make_task("child", dependencies=["nonexistent"]))
        with pytest.raises(ValueError, match="unknown task 'nonexistent'"):
            scheduler.validate()

    def test_validate_simple_cycle(self):
        """Simple 2-node cycle is detected."""
        scheduler = DependencyScheduler()
        scheduler.add_task(_make_task("a", dependencies=["b"]))
        scheduler.add_task(_make_task("b", dependencies=["a"]))
        with pytest.raises(CyclicDependencyError, match="Cyclic dependency"):
            scheduler.validate()

    def test_validate_self_cycle(self):
        """Self-referencing dependency is detected."""
        scheduler = DependencyScheduler()
        scheduler.add_task(_make_task("a", dependencies=["a"]))
        with pytest.raises(CyclicDependencyError, match="Cyclic dependency"):
            scheduler.validate()

    def test_validate_long_cycle(self):
        """Long cycle (A -> B -> C -> A) is detected."""
        scheduler = DependencyScheduler()
        scheduler.add_task(_make_task("a", dependencies=["c"]))
        scheduler.add_task(_make_task("b", dependencies=["a"]))
        scheduler.add_task(_make_task("c", dependencies=["b"]))
        with pytest.raises(CyclicDependencyError, match="Cyclic dependency"):
            scheduler.validate()

    def test_validate_diamond_no_cycle(self):
        """Diamond dependency (no cycle) passes validation."""
        scheduler = DependencyScheduler()
        scheduler.add_task(_make_task("root"))
        scheduler.add_task(_make_task("left", dependencies=["root"]))
        scheduler.add_task(_make_task("right", dependencies=["root"]))
        scheduler.add_task(_make_task("bottom", dependencies=["left", "right"]))
        scheduler.validate()  # Diamond is valid, not a cycle


class TestDependencySchedulerReadyTasks:
    """get_ready_tasks() behavior."""

    def test_no_deps_all_ready(self):
        """Tasks with no dependencies are immediately ready."""
        scheduler = DependencyScheduler()
        scheduler.add_task(_make_task("a"))
        scheduler.add_task(_make_task("b"))
        scheduler.add_task(_make_task("c"))
        ready = scheduler.get_ready_tasks()
        names = {t.name for t in ready}
        assert names == {"a", "b", "c"}

    def test_deps_not_ready(self):
        """Tasks with unsatisfied dependencies are not ready."""
        scheduler = DependencyScheduler()
        scheduler.add_task(_make_task("parent"))
        scheduler.add_task(_make_task("child", dependencies=["parent"]))
        ready = scheduler.get_ready_tasks()
        names = {t.name for t in ready}
        assert names == {"parent"}  # Only parent is ready

    def test_deps_satisfied_after_done(self):
        """Tasks become ready when dependencies are marked DONE."""
        scheduler = DependencyScheduler()
        scheduler.add_task(_make_task("parent"))
        scheduler.add_task(_make_task("child", dependencies=["parent"]))

        # Initially only parent is ready
        ready_names = {t.name for t in scheduler.get_ready_tasks()}
        assert ready_names == {"parent"}

        # Mark parent as done
        scheduler.mark_phase("parent", TaskPhase.DONE)

        # Now child should be ready
        ready_names = {t.name for t in scheduler.get_ready_tasks()}
        assert ready_names == {"child"}

    def test_intermediate_phases_not_ready(self):
        """Dependencies in DOWNLOADING/UNPACKING/INSTALLING don't satisfy."""
        scheduler = DependencyScheduler()
        scheduler.add_task(_make_task("parent"))
        scheduler.add_task(_make_task("child", dependencies=["parent"]))

        for phase in [TaskPhase.DOWNLOADING, TaskPhase.UNPACKING, TaskPhase.INSTALLING]:
            scheduler.mark_phase("parent", phase)
            ready_names = {t.name for t in scheduler.get_ready_tasks()}
            assert "child" not in ready_names, f"child should not be ready when parent is {phase}"

    def test_non_waiting_not_returned(self):
        """Tasks already in progress are not returned as ready."""
        scheduler = DependencyScheduler()
        scheduler.add_task(_make_task("a"))
        scheduler.mark_phase("a", TaskPhase.DOWNLOADING)
        ready = scheduler.get_ready_tasks()
        assert len(ready) == 0

    def test_chain_dependency(self):
        """A -> B -> C: tasks become ready one at a time."""
        scheduler = DependencyScheduler()
        scheduler.add_task(_make_task("a"))
        scheduler.add_task(_make_task("b", dependencies=["a"]))
        scheduler.add_task(_make_task("c", dependencies=["b"]))

        # Only a is ready
        assert {t.name for t in scheduler.get_ready_tasks()} == {"a"}

        scheduler.mark_phase("a", TaskPhase.DONE)
        assert {t.name for t in scheduler.get_ready_tasks()} == {"b"}

        scheduler.mark_phase("b", TaskPhase.DONE)
        assert {t.name for t in scheduler.get_ready_tasks()} == {"c"}

    def test_avr_dependency_graph(self):
        """Simulates real AVR package dependency graph."""
        scheduler = DependencyScheduler()
        scheduler.add_task(_make_task("platform-atmelavr"))
        scheduler.add_task(_make_task("toolchain-atmelavr", dependencies=["platform-atmelavr"]))
        scheduler.add_task(_make_task("framework-arduino-avr", dependencies=["toolchain-atmelavr"]))
        scheduler.add_task(_make_task("Wire", dependencies=["framework-arduino-avr"]))
        scheduler.add_task(_make_task("SPI", dependencies=["framework-arduino-avr"]))
        scheduler.add_task(_make_task("Servo", dependencies=["framework-arduino-avr"]))
        scheduler.validate()

        # Only platform is ready initially
        assert {t.name for t in scheduler.get_ready_tasks()} == {"platform-atmelavr"}

        # After platform done, toolchain is ready
        scheduler.mark_phase("platform-atmelavr", TaskPhase.DONE)
        assert {t.name for t in scheduler.get_ready_tasks()} == {"toolchain-atmelavr"}

        # After toolchain done, framework is ready
        scheduler.mark_phase("toolchain-atmelavr", TaskPhase.DONE)
        assert {t.name for t in scheduler.get_ready_tasks()} == {"framework-arduino-avr"}

        # After framework done, all libraries are ready in parallel
        scheduler.mark_phase("framework-arduino-avr", TaskPhase.DONE)
        ready_names = {t.name for t in scheduler.get_ready_tasks()}
        assert ready_names == {"Wire", "SPI", "Servo"}


class TestDependencySchedulerPhaseManagement:
    """mark_phase() and state queries."""

    def test_mark_phase(self):
        """mark_phase() updates the task's phase."""
        scheduler = DependencyScheduler()
        scheduler.add_task(_make_task("a"))
        scheduler.mark_phase("a", TaskPhase.DOWNLOADING)
        task = scheduler.get_task("a")
        assert task.phase == TaskPhase.DOWNLOADING

    def test_mark_unknown_task_raises(self):
        """Marking a non-existent task raises KeyError."""
        scheduler = DependencyScheduler()
        with pytest.raises(KeyError, match="Unknown task"):
            scheduler.mark_phase("nonexistent", TaskPhase.DONE)

    def test_all_done_true(self):
        """all_done() returns True when all tasks are DONE or FAILED."""
        scheduler = DependencyScheduler()
        scheduler.add_task(_make_task("a"))
        scheduler.add_task(_make_task("b"))
        scheduler.mark_phase("a", TaskPhase.DONE)
        scheduler.mark_phase("b", TaskPhase.FAILED)
        assert scheduler.all_done() is True

    def test_all_done_false(self):
        """all_done() returns False when some tasks are still in progress."""
        scheduler = DependencyScheduler()
        scheduler.add_task(_make_task("a"))
        scheduler.add_task(_make_task("b"))
        scheduler.mark_phase("a", TaskPhase.DONE)
        assert scheduler.all_done() is False

    def test_all_done_empty(self):
        """all_done() returns True for empty scheduler."""
        scheduler = DependencyScheduler()
        assert scheduler.all_done() is True

    def test_has_failed(self):
        """has_failed() detects FAILED tasks."""
        scheduler = DependencyScheduler()
        scheduler.add_task(_make_task("a"))
        assert scheduler.has_failed() is False
        scheduler.mark_phase("a", TaskPhase.FAILED)
        assert scheduler.has_failed() is True


class TestDependencySchedulerBlockedTasks:
    """get_blocked_tasks() behavior."""

    def test_no_blocked_when_all_ok(self):
        """No blocked tasks when nothing has failed."""
        scheduler = DependencyScheduler()
        scheduler.add_task(_make_task("parent"))
        scheduler.add_task(_make_task("child", dependencies=["parent"]))
        assert scheduler.get_blocked_tasks() == []

    def test_child_blocked_by_failed_parent(self):
        """A WAITING child with a FAILED parent is blocked."""
        scheduler = DependencyScheduler()
        scheduler.add_task(_make_task("parent"))
        scheduler.add_task(_make_task("child", dependencies=["parent"]))
        scheduler.mark_phase("parent", TaskPhase.FAILED)
        blocked = scheduler.get_blocked_tasks()
        assert len(blocked) == 1
        assert blocked[0].name == "child"

    def test_grandchild_not_directly_blocked(self):
        """Only direct children of failed tasks are blocked (not grandchildren that have intermediate WAITING deps)."""
        scheduler = DependencyScheduler()
        scheduler.add_task(_make_task("a"))
        scheduler.add_task(_make_task("b", dependencies=["a"]))
        scheduler.add_task(_make_task("c", dependencies=["b"]))
        scheduler.mark_phase("a", TaskPhase.FAILED)
        blocked = scheduler.get_blocked_tasks()
        # Only b is directly blocked (its dep 'a' is FAILED)
        # c is waiting on b which is WAITING, not FAILED
        blocked_names = {t.name for t in blocked}
        assert blocked_names == {"b"}


class TestDependencySchedulerSerialization:
    """to_dict() serialization."""

    def test_to_dict(self):
        """Scheduler state can be serialized."""
        scheduler = DependencyScheduler()
        scheduler.add_task(_make_task("a"))
        scheduler.add_task(_make_task("b", dependencies=["a"]))
        scheduler.mark_phase("a", TaskPhase.DOWNLOADING)

        d = scheduler.to_dict()
        assert "tasks" in d
        assert "a" in d["tasks"]
        assert "b" in d["tasks"]
        assert d["tasks"]["a"]["phase"] == "downloading"
        assert d["tasks"]["b"]["phase"] == "waiting"
        assert d["tasks"]["b"]["dependencies"] == ["a"]


class TestDependencySchedulerThreadSafety:
    """Thread-safety of concurrent mark_phase() calls."""

    def test_concurrent_mark_phase(self):
        """Multiple threads can call mark_phase() concurrently without error."""
        scheduler = DependencyScheduler()
        num_tasks = 50
        for i in range(num_tasks):
            scheduler.add_task(_make_task(f"task-{i}"))

        errors: list[Exception] = []
        barrier = threading.Barrier(num_tasks)

        def worker(task_name: str) -> None:
            try:
                barrier.wait(timeout=5)
                scheduler.mark_phase(task_name, TaskPhase.DOWNLOADING)
                scheduler.mark_phase(task_name, TaskPhase.UNPACKING)
                scheduler.mark_phase(task_name, TaskPhase.INSTALLING)
                scheduler.mark_phase(task_name, TaskPhase.DONE)
            except Exception as e:
                errors.append(e)

        threads = [threading.Thread(target=worker, args=(f"task-{i}",)) for i in range(num_tasks)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=10)

        assert len(errors) == 0, f"Thread errors: {errors}"
        assert scheduler.all_done() is True

    def test_concurrent_get_ready_and_mark(self):
        """get_ready_tasks() and mark_phase() can run concurrently."""
        scheduler = DependencyScheduler()
        scheduler.add_task(_make_task("root"))
        for i in range(10):
            scheduler.add_task(_make_task(f"child-{i}", dependencies=["root"]))

        errors: list[Exception] = []

        def reader() -> None:
            """Repeatedly call get_ready_tasks()."""
            try:
                for _ in range(100):
                    scheduler.get_ready_tasks()
            except Exception as e:
                errors.append(e)

        def writer() -> None:
            """Mark root as done after a brief delay."""
            try:
                scheduler.mark_phase("root", TaskPhase.DONE)
            except Exception as e:
                errors.append(e)

        reader_thread = threading.Thread(target=reader)
        writer_thread = threading.Thread(target=writer)
        reader_thread.start()
        writer_thread.start()
        reader_thread.join(timeout=10)
        writer_thread.join(timeout=10)

        assert len(errors) == 0, f"Thread errors: {errors}"
