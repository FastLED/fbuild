"""Unit tests for the pipeline progress display (Rich-based TUI renderer).

Tests cover:
- PipelineProgressDisplay implements ProgressCallback
- Task registration and ordering
- Phase transitions update display state correctly
- Progress bar formatting for download/unpack phases
- Status text formatting for all phases
- Footer summary counts (total, active, done, failed)
- Thread-safety: concurrent on_progress calls
- Snapshot API for testing
- Context manager usage
- Console output capture with Rich StringIO
"""

import threading
from io import StringIO

from rich.console import Console

from fbuild.packages.pipeline.callbacks import ProgressCallback
from fbuild.packages.pipeline.models import TaskPhase
from fbuild.packages.pipeline.progress_display import (
    PipelineProgressDisplay,
    _TaskDisplayState,
)


class TestTaskDisplayState:
    """Tests for the internal _TaskDisplayState class."""

    def test_initial_state(self) -> None:
        """New state should be WAITING with zero progress."""
        state = _TaskDisplayState("test-pkg", "1.0.0")
        assert state.name == "test-pkg"
        assert state.version == "1.0.0"
        assert state.phase == TaskPhase.WAITING
        assert state.progress == 0.0
        assert state.total == 0.0
        assert state.detail == ""
        assert state.elapsed == 0.0
        assert state.start_time is None

    def test_empty_version(self) -> None:
        """State should work with empty version string."""
        state = _TaskDisplayState("pkg", "")
        assert state.version == ""


class TestProgressDisplayProtocol:
    """Tests that PipelineProgressDisplay satisfies ProgressCallback."""

    def test_implements_protocol(self) -> None:
        """PipelineProgressDisplay should implement ProgressCallback."""
        display = PipelineProgressDisplay(
            console=Console(file=StringIO()),
            env_name="uno",
            refresh_per_second=4,
        )
        assert isinstance(display, ProgressCallback)

    def test_on_progress_callable(self) -> None:
        """on_progress should be callable with correct signature."""
        display = PipelineProgressDisplay(
            console=Console(file=StringIO()),
            env_name="uno",
            refresh_per_second=4,
        )
        # Should not raise
        display.on_progress("pkg", TaskPhase.DOWNLOADING, 50.0, 100.0, "50%")


class TestTaskRegistration:
    """Tests for task registration and ordering."""

    def test_register_single_task(self) -> None:
        """Registering a task should make it visible in snapshot."""
        display = PipelineProgressDisplay(
            console=Console(file=StringIO()),
            env_name="uno",
            refresh_per_second=4,
        )
        display.register_task("pkg-a", "1.0")
        snap = display.get_snapshot()
        assert len(snap) == 1
        assert snap[0]["name"] == "pkg-a"
        assert snap[0]["version"] == "1.0"
        assert snap[0]["phase"] == TaskPhase.WAITING

    def test_register_multiple_tasks_preserves_order(self) -> None:
        """Tasks should appear in registration order."""
        display = PipelineProgressDisplay(
            console=Console(file=StringIO()),
            env_name="uno",
            refresh_per_second=4,
        )
        display.register_task("alpha", "1.0")
        display.register_task("beta", "2.0")
        display.register_task("gamma", "3.0")
        snap = display.get_snapshot()
        assert [s["name"] for s in snap] == ["alpha", "beta", "gamma"]

    def test_register_duplicate_task_ignored(self) -> None:
        """Registering the same task name twice should not create duplicates."""
        display = PipelineProgressDisplay(
            console=Console(file=StringIO()),
            env_name="uno",
            refresh_per_second=4,
        )
        display.register_task("pkg", "1.0")
        display.register_task("pkg", "2.0")
        snap = display.get_snapshot()
        assert len(snap) == 1
        assert snap[0]["version"] == "1.0"  # First registration wins

    def test_unregistered_task_auto_registered_on_progress(self) -> None:
        """on_progress for unregistered task should auto-register it."""
        display = PipelineProgressDisplay(
            console=Console(file=StringIO()),
            env_name="uno",
            refresh_per_second=4,
        )
        display.on_progress("new-pkg", TaskPhase.DOWNLOADING, 10, 100, "Starting")
        snap = display.get_snapshot()
        assert len(snap) == 1
        assert snap[0]["name"] == "new-pkg"
        assert snap[0]["phase"] == TaskPhase.DOWNLOADING


class TestPhaseTransitions:
    """Tests for phase transition handling."""

    def test_waiting_to_downloading(self) -> None:
        """Transition from WAITING to DOWNLOADING should update state."""
        display = PipelineProgressDisplay(
            console=Console(file=StringIO()),
            env_name="test",
            refresh_per_second=4,
        )
        display.register_task("pkg", "1.0")
        display.on_progress("pkg", TaskPhase.DOWNLOADING, 0, 1000, "Queued")
        snap = display.get_snapshot()
        assert snap[0]["phase"] == TaskPhase.DOWNLOADING

    def test_downloading_to_unpacking(self) -> None:
        """Transition from DOWNLOADING to UNPACKING should update state."""
        display = PipelineProgressDisplay(
            console=Console(file=StringIO()),
            env_name="test",
            refresh_per_second=4,
        )
        display.register_task("pkg", "1.0")
        display.on_progress("pkg", TaskPhase.DOWNLOADING, 1000, 1000, "Complete")
        display.on_progress("pkg", TaskPhase.UNPACKING, 0, 50, "Starting extraction")
        snap = display.get_snapshot()
        assert snap[0]["phase"] == TaskPhase.UNPACKING

    def test_unpacking_to_installing(self) -> None:
        """Transition from UNPACKING to INSTALLING should update state."""
        display = PipelineProgressDisplay(
            console=Console(file=StringIO()),
            env_name="test",
            refresh_per_second=4,
        )
        display.register_task("pkg", "1.0")
        display.on_progress("pkg", TaskPhase.UNPACKING, 50, 50, "Done")
        display.on_progress("pkg", TaskPhase.INSTALLING, 0, 3, "Verifying...")
        snap = display.get_snapshot()
        assert snap[0]["phase"] == TaskPhase.INSTALLING

    def test_installing_to_done(self) -> None:
        """Transition to DONE should record elapsed time."""
        display = PipelineProgressDisplay(
            console=Console(file=StringIO()),
            env_name="test",
            refresh_per_second=4,
        )
        display.register_task("pkg", "1.0")
        display.on_progress("pkg", TaskPhase.DOWNLOADING, 0, 100, "Start")
        display.on_progress("pkg", TaskPhase.DONE, 1, 1, "Done")
        snap = display.get_snapshot()
        assert snap[0]["phase"] == TaskPhase.DONE
        assert snap[0]["elapsed"] >= 0

    def test_transition_to_failed(self) -> None:
        """Transition to FAILED should store error detail."""
        display = PipelineProgressDisplay(
            console=Console(file=StringIO()),
            env_name="test",
            refresh_per_second=4,
        )
        display.register_task("pkg", "1.0")
        display.on_progress("pkg", TaskPhase.DOWNLOADING, 50, 100, "In progress")
        display.on_progress("pkg", TaskPhase.FAILED, 0, 0, "Network timeout")
        snap = display.get_snapshot()
        assert snap[0]["phase"] == TaskPhase.FAILED
        assert snap[0]["detail"] == "Network timeout"

    def test_full_lifecycle(self) -> None:
        """Task should transition through all phases correctly."""
        display = PipelineProgressDisplay(
            console=Console(file=StringIO()),
            env_name="test",
            refresh_per_second=4,
        )
        display.register_task("pkg", "1.0")

        phases = [
            (TaskPhase.DOWNLOADING, 0, 100, "Start"),
            (TaskPhase.DOWNLOADING, 50, 100, "2.1 MB/s"),
            (TaskPhase.DOWNLOADING, 100, 100, "Complete"),
            (TaskPhase.UNPACKING, 0, 50, "Extracting"),
            (TaskPhase.UNPACKING, 50, 50, "Done"),
            (TaskPhase.INSTALLING, 0, 3, "Verifying"),
            (TaskPhase.INSTALLING, 3, 3, "Complete"),
            (TaskPhase.DONE, 1, 1, "Done"),
        ]

        for phase, progress, total, detail in phases:
            display.on_progress("pkg", phase, progress, total, detail)

        snap = display.get_snapshot()
        assert snap[0]["phase"] == TaskPhase.DONE


class TestProgressBarFormatting:
    """Tests for progress bar text rendering."""

    def test_zero_progress_bar(self) -> None:
        """0% progress should show empty bar."""
        display = PipelineProgressDisplay(
            console=Console(file=StringIO()),
            env_name="test",
            refresh_per_second=4,
        )
        display.register_task("pkg", "1.0")
        display.on_progress("pkg", TaskPhase.DOWNLOADING, 0, 1000, "Starting")

        # Access internal rendering
        state = display._states["pkg"]
        text = display._format_progress_bar(state)
        text_str = text.plain
        assert "0%" in text_str
        assert "[" in text_str and "]" in text_str

    def test_full_progress_bar(self) -> None:
        """100% progress should show full bar."""
        display = PipelineProgressDisplay(
            console=Console(file=StringIO()),
            env_name="test",
            refresh_per_second=4,
        )
        display.register_task("pkg", "1.0")
        display.on_progress("pkg", TaskPhase.DOWNLOADING, 1000, 1000, "Complete")

        state = display._states["pkg"]
        text = display._format_progress_bar(state)
        text_str = text.plain
        assert "100%" in text_str
        assert "=" * 20 in text_str

    def test_half_progress_bar(self) -> None:
        """50% progress should show half-filled bar."""
        display = PipelineProgressDisplay(
            console=Console(file=StringIO()),
            env_name="test",
            refresh_per_second=4,
        )
        display.register_task("pkg", "1.0")
        display.on_progress("pkg", TaskPhase.DOWNLOADING, 500, 1000, "In progress")

        state = display._states["pkg"]
        text = display._format_progress_bar(state)
        text_str = text.plain
        assert "50%" in text_str

    def test_unknown_total_shows_zero(self) -> None:
        """Unknown total (0) should show 0% bar."""
        display = PipelineProgressDisplay(
            console=Console(file=StringIO()),
            env_name="test",
            refresh_per_second=4,
        )
        display.register_task("pkg", "1.0")
        display.on_progress("pkg", TaskPhase.DOWNLOADING, 0, 0, "Unknown size")

        state = display._states["pkg"]
        text = display._format_progress_bar(state)
        text_str = text.plain
        assert "0%" in text_str

    def test_download_shows_speed(self) -> None:
        """Download bar should include transfer speed when available."""
        display = PipelineProgressDisplay(
            console=Console(file=StringIO()),
            env_name="test",
            refresh_per_second=4,
        )
        display.register_task("pkg", "1.0")
        display.on_progress("pkg", TaskPhase.DOWNLOADING, 500, 1000, "2.1 MB/s")

        state = display._states["pkg"]
        text = display._format_progress_bar(state)
        text_str = text.plain
        assert "2.1 MB/s" in text_str


class TestStatusFormatting:
    """Tests for status column rendering across phases."""

    def _make_display(self) -> PipelineProgressDisplay:
        return PipelineProgressDisplay(
            console=Console(file=StringIO()),
            env_name="test",
            refresh_per_second=4,
        )

    def test_waiting_status_empty(self) -> None:
        """WAITING phase should show empty status."""
        display = self._make_display()
        display.register_task("pkg", "1.0")
        state = display._states["pkg"]
        text = display._format_status(state)
        assert text.plain == ""

    def test_installing_shows_spinner_and_detail(self) -> None:
        """INSTALLING phase should show braille spinner and detail."""
        display = self._make_display()
        display.register_task("pkg", "1.0")
        display.on_progress("pkg", TaskPhase.INSTALLING, 1, 3, "Verifying binaries...")
        state = display._states["pkg"]
        text = display._format_status(state)
        assert "Verifying binaries..." in text.plain
        # Should contain one of the braille spinner characters
        spinner_chars = set("\u280b\u2819\u2839\u2838\u283c\u2834\u2826\u2827\u2807\u280f")
        assert any(c in text.plain for c in spinner_chars), f"Expected spinner char in: {text.plain!r}"

    def test_done_shows_checkmark_and_elapsed(self) -> None:
        """DONE phase should show Unicode checkmark and elapsed time."""
        display = self._make_display()
        display.register_task("pkg", "1.0")
        display.on_progress("pkg", TaskPhase.DOWNLOADING, 0, 100, "Start")
        display.on_progress("pkg", TaskPhase.DONE, 1, 1, "Complete")
        state = display._states["pkg"]
        text = display._format_status(state)
        assert "\u2713" in text.plain  # Unicode checkmark ✓

    def test_failed_shows_error(self) -> None:
        """FAILED phase should show Unicode cross and error message."""
        display = self._make_display()
        display.register_task("pkg", "1.0")
        display.on_progress("pkg", TaskPhase.FAILED, 0, 0, "Connection refused")
        state = display._states["pkg"]
        text = display._format_status(state)
        assert "\u2717" in text.plain  # Unicode cross ✗
        assert "Connection refused" in text.plain


class TestPhaseFormatting:
    """Tests for phase column styling."""

    def _make_display(self) -> PipelineProgressDisplay:
        return PipelineProgressDisplay(
            console=Console(file=StringIO()),
            env_name="test",
            refresh_per_second=4,
        )

    def test_all_phases_have_labels(self) -> None:
        """Every TaskPhase should produce a non-empty phase label."""
        display = self._make_display()
        display.register_task("pkg", "1.0")
        for phase in TaskPhase:
            display.on_progress("pkg", phase, 0, 0, "")
            state = display._states["pkg"]
            text = display._format_phase(state)
            assert text.plain.strip() != "", f"Phase {phase} has no label"

    def test_waiting_label(self) -> None:
        """WAITING should show 'Waiting'."""
        display = self._make_display()
        display.register_task("pkg", "1.0")
        state = display._states["pkg"]
        text = display._format_phase(state)
        assert text.plain == "Waiting"

    def test_downloading_label(self) -> None:
        """DOWNLOADING should show 'Downloading'."""
        display = self._make_display()
        display.register_task("pkg", "1.0")
        display.on_progress("pkg", TaskPhase.DOWNLOADING, 0, 100, "")
        state = display._states["pkg"]
        text = display._format_phase(state)
        assert text.plain == "Downloading"

    def test_done_label(self) -> None:
        """DONE should show 'Done'."""
        display = self._make_display()
        display.register_task("pkg", "1.0")
        display.on_progress("pkg", TaskPhase.DONE, 1, 1, "")
        state = display._states["pkg"]
        text = display._format_phase(state)
        assert text.plain == "Done"

    def test_failed_label(self) -> None:
        """FAILED should show 'Failed'."""
        display = self._make_display()
        display.register_task("pkg", "1.0")
        display.on_progress("pkg", TaskPhase.FAILED, 0, 0, "err")
        state = display._states["pkg"]
        text = display._format_phase(state)
        assert text.plain == "Failed"


class TestNameFormatting:
    """Tests for name column styling by phase."""

    def _make_display(self) -> PipelineProgressDisplay:
        return PipelineProgressDisplay(
            console=Console(file=StringIO()),
            env_name="test",
            refresh_per_second=4,
        )

    def test_waiting_name_includes_version(self) -> None:
        """WAITING task should show name and version."""
        display = self._make_display()
        display.register_task("my-pkg", "2.3.1")
        state = display._states["my-pkg"]
        text = display._format_name(state)
        assert "my-pkg" in text.plain
        assert "2.3.1" in text.plain

    def test_name_without_version(self) -> None:
        """Task with empty version should show name only."""
        display = self._make_display()
        display.register_task("pkg", "")
        state = display._states["pkg"]
        text = display._format_name(state)
        assert text.plain == "pkg"


class TestFooterSummary:
    """Tests for the footer summary line in the rendered display."""

    def _make_display(self) -> PipelineProgressDisplay:
        return PipelineProgressDisplay(
            console=Console(file=StringIO()),
            env_name="test",
            refresh_per_second=4,
        )

    def test_footer_total_count(self) -> None:
        """Footer should show total package count."""
        display = self._make_display()
        display.register_task("a", "1.0")
        display.register_task("b", "2.0")
        display.register_task("c", "3.0")

        output = StringIO()
        console = Console(file=output, force_terminal=True, width=120)
        display._console = console
        footer = display._render_footer()
        console.print(footer)
        text = output.getvalue()
        assert "3 packages" in text

    def test_footer_active_count(self) -> None:
        """Footer should show active tasks count."""
        display = self._make_display()
        display.register_task("a", "1.0")
        display.register_task("b", "2.0")
        display.on_progress("a", TaskPhase.DOWNLOADING, 50, 100, "")

        output = StringIO()
        console = Console(file=output, force_terminal=True, width=120)
        display._console = console
        footer = display._render_footer()
        console.print(footer)
        text = output.getvalue()
        assert "1 active" in text

    def test_footer_done_count(self) -> None:
        """Footer should show done tasks count."""
        display = self._make_display()
        display.register_task("a", "1.0")
        display.register_task("b", "2.0")
        display.on_progress("a", TaskPhase.DONE, 1, 1, "")
        display.on_progress("b", TaskPhase.DONE, 1, 1, "")

        output = StringIO()
        console = Console(file=output, force_terminal=True, width=120)
        display._console = console
        footer = display._render_footer()
        console.print(footer)
        text = output.getvalue()
        assert "2 done" in text

    def test_footer_failed_count(self) -> None:
        """Footer should show failed tasks count."""
        display = self._make_display()
        display.register_task("a", "1.0")
        display.on_progress("a", TaskPhase.FAILED, 0, 0, "Error")

        output = StringIO()
        console = Console(file=output, force_terminal=True, width=120)
        display._console = console
        footer = display._render_footer()
        console.print(footer)
        text = output.getvalue()
        assert "1 failed" in text


class TestTableRendering:
    """Tests for complete table rendering via Rich Console capture."""

    def test_render_empty_table(self) -> None:
        """Empty display should render without error."""
        display = PipelineProgressDisplay(
            console=Console(file=StringIO()),
            env_name="test",
            refresh_per_second=4,
        )
        table = display._render_table()
        # Should not raise
        output = StringIO()
        Console(file=output, width=120).print(table)

    def test_render_single_waiting_task(self) -> None:
        """Single WAITING task should render name and Waiting phase."""
        output = StringIO()
        console = Console(file=output, force_terminal=True, width=120)
        display = PipelineProgressDisplay(
            console=console,
            env_name="test",
            refresh_per_second=4,
        )
        display.register_task("Wire", "1.0")
        table = display._render_table()
        console.print(table)
        text = output.getvalue()
        assert "Wire" in text
        assert "Waiting" in text

    def test_render_downloading_task_shows_bar(self) -> None:
        """Downloading task should show progress bar."""
        output = StringIO()
        console = Console(file=output, force_terminal=True, width=120)
        display = PipelineProgressDisplay(
            console=console,
            env_name="test",
            refresh_per_second=4,
        )
        display.register_task("atmelavr", "5.0.0")
        display.on_progress("atmelavr", TaskPhase.DOWNLOADING, 4500, 10000, "2.1 MB/s")
        table = display._render_table()
        console.print(table)
        text = output.getvalue()
        assert "atmelavr" in text
        assert "Downloading" in text
        assert "45%" in text

    def test_render_done_task(self) -> None:
        """Done task should show Done phase."""
        output = StringIO()
        console = Console(file=output, force_terminal=True, width=120)
        display = PipelineProgressDisplay(
            console=console,
            env_name="test",
            refresh_per_second=4,
        )
        display.register_task("SPI", "1.0")
        display.on_progress("SPI", TaskPhase.DOWNLOADING, 0, 100, "Start")
        display.on_progress("SPI", TaskPhase.DONE, 1, 1, "Done")
        table = display._render_table()
        console.print(table)
        text = output.getvalue()
        assert "SPI" in text
        assert "Done" in text

    def test_render_mixed_phases(self) -> None:
        """Multiple tasks in different phases should all render."""
        output = StringIO()
        console = Console(file=output, force_terminal=True, width=120)
        display = PipelineProgressDisplay(
            console=console,
            env_name="uno",
            refresh_per_second=4,
        )
        display.register_task("atmelavr", "5.0.0")
        display.register_task("toolchain-atmelavr", "3.1")
        display.register_task("framework-arduino", "4.2.0")
        display.register_task("Wire", "1.0")
        display.register_task("SPI", "1.0")
        display.register_task("Servo", "1.1.8")

        display.on_progress("atmelavr", TaskPhase.DOWNLOADING, 4500, 10000, "2.1 MB/s")
        display.on_progress("toolchain-atmelavr", TaskPhase.UNPACKING, 39, 50, "Extracting")
        display.on_progress("framework-arduino", TaskPhase.INSTALLING, 1, 3, "Verifying binaries...")
        display.on_progress("Wire", TaskPhase.DOWNLOADING, 0, 100, "Start")
        display.on_progress("Wire", TaskPhase.DONE, 1, 1, "Done in 1.2s")
        display.on_progress("SPI", TaskPhase.DOWNLOADING, 0, 100, "Start")
        display.on_progress("SPI", TaskPhase.DONE, 1, 1, "Done in 0.8s")
        # Servo stays WAITING

        group = display._render_display()
        console.print(group)
        text = output.getvalue()

        # All names present
        assert "atmelavr" in text
        assert "toolchain-atmelavr" in text
        assert "framework-arduino" in text
        assert "Wire" in text
        assert "SPI" in text
        assert "Servo" in text

        # Mixed phases
        assert "Downloading" in text
        assert "Unpacking" in text
        assert "Installing" in text
        assert "Done" in text
        assert "Waiting" in text

        # Header
        assert "Installing dependencies for env:uno..." in text

        # Footer
        assert "6 packages" in text


class TestThreadSafety:
    """Tests for thread-safe concurrent access to the display."""

    def test_concurrent_on_progress_calls(self) -> None:
        """Multiple threads calling on_progress should not corrupt state."""
        display = PipelineProgressDisplay(
            console=Console(file=StringIO()),
            env_name="test",
            refresh_per_second=4,
        )

        # Register tasks
        for i in range(10):
            display.register_task(f"pkg-{i}", f"{i}.0")

        errors: list[Exception] = []

        def update_task(task_idx: int) -> None:
            try:
                name = f"pkg-{task_idx}"
                for progress in range(0, 101, 10):
                    display.on_progress(name, TaskPhase.DOWNLOADING, float(progress), 100.0, f"{progress}%")
                display.on_progress(name, TaskPhase.UNPACKING, 0, 50, "Extracting")
                for progress in range(0, 51, 5):
                    display.on_progress(name, TaskPhase.UNPACKING, float(progress), 50.0, f"{progress}/50")
                display.on_progress(name, TaskPhase.INSTALLING, 0, 3, "Verifying")
                display.on_progress(name, TaskPhase.DONE, 1, 1, "Complete")
            except Exception as e:
                errors.append(e)

        threads = [threading.Thread(target=update_task, args=(i,)) for i in range(10)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=10)

        assert len(errors) == 0, f"Thread errors: {errors}"

        # All tasks should be DONE
        snap = display.get_snapshot()
        assert len(snap) == 10
        for s in snap:
            assert s["phase"] == TaskPhase.DONE

    def test_concurrent_register_and_progress(self) -> None:
        """Registering tasks while updating others should be thread-safe."""
        display = PipelineProgressDisplay(
            console=Console(file=StringIO()),
            env_name="test",
            refresh_per_second=4,
        )

        errors: list[Exception] = []

        def register_tasks() -> None:
            try:
                for i in range(20):
                    display.register_task(f"reg-{i}", f"{i}.0")
            except Exception as e:
                errors.append(e)

        def progress_tasks() -> None:
            try:
                for i in range(20):
                    display.on_progress(f"prog-{i}", TaskPhase.DOWNLOADING, 0, 100, "")
            except Exception as e:
                errors.append(e)

        t1 = threading.Thread(target=register_tasks)
        t2 = threading.Thread(target=progress_tasks)
        t1.start()
        t2.start()
        t1.join(timeout=10)
        t2.join(timeout=10)

        assert len(errors) == 0, f"Thread errors: {errors}"
        snap = display.get_snapshot()
        assert len(snap) == 40  # 20 registered + 20 auto-registered


class TestSnapshot:
    """Tests for the get_snapshot() testing API."""

    def test_snapshot_returns_list_of_dicts(self) -> None:
        """Snapshot should return list of dictionaries."""
        display = PipelineProgressDisplay(
            console=Console(file=StringIO()),
            env_name="test",
            refresh_per_second=4,
        )
        display.register_task("pkg", "1.0")
        snap = display.get_snapshot()
        assert isinstance(snap, list)
        assert len(snap) == 1
        assert isinstance(snap[0], dict)

    def test_snapshot_fields(self) -> None:
        """Snapshot dict should contain expected fields."""
        display = PipelineProgressDisplay(
            console=Console(file=StringIO()),
            env_name="test",
            refresh_per_second=4,
        )
        display.register_task("pkg", "1.0")
        snap = display.get_snapshot()
        expected_keys = {"name", "version", "phase", "progress", "total", "detail", "elapsed"}
        assert set(snap[0].keys()) == expected_keys

    def test_snapshot_reflects_updates(self) -> None:
        """Snapshot should reflect latest on_progress calls."""
        display = PipelineProgressDisplay(
            console=Console(file=StringIO()),
            env_name="test",
            refresh_per_second=4,
        )
        display.register_task("pkg", "1.0")
        display.on_progress("pkg", TaskPhase.DOWNLOADING, 50, 100, "50%")
        snap = display.get_snapshot()
        assert snap[0]["phase"] == TaskPhase.DOWNLOADING
        assert snap[0]["progress"] == 50
        assert snap[0]["total"] == 100
        assert snap[0]["detail"] == "50%"

    def test_empty_snapshot(self) -> None:
        """Snapshot of empty display should return empty list."""
        display = PipelineProgressDisplay(
            console=Console(file=StringIO()),
            env_name="test",
            refresh_per_second=4,
        )
        snap = display.get_snapshot()
        assert snap == []


class TestContextManager:
    """Tests for context manager usage (start/stop Live display)."""

    def test_context_manager_starts_and_stops(self) -> None:
        """Context manager should start and stop the Live display."""
        output = StringIO()
        console = Console(file=output, force_terminal=True, width=120)
        display = PipelineProgressDisplay(
            console=console,
            env_name="test",
            refresh_per_second=4,
        )
        display.register_task("pkg", "1.0")

        with display:
            assert display._live is not None
            display.on_progress("pkg", TaskPhase.DOWNLOADING, 50, 100, "50%")
            display.update()

        assert display._live is None

    def test_start_stop_explicit(self) -> None:
        """Explicit start/stop should work like context manager."""
        output = StringIO()
        console = Console(file=output, force_terminal=True, width=120)
        display = PipelineProgressDisplay(
            console=console,
            env_name="test",
            refresh_per_second=4,
        )
        display.register_task("pkg", "1.0")

        display.start()
        assert display._live is not None
        display.update()
        display.stop()
        assert display._live is None

    def test_stop_without_start(self) -> None:
        """Stopping without starting should not raise."""
        display = PipelineProgressDisplay(
            console=Console(file=StringIO()),
            env_name="test",
            refresh_per_second=4,
        )
        display.stop()  # Should not raise

    def test_double_stop(self) -> None:
        """Calling stop twice should not raise."""
        output = StringIO()
        console = Console(file=output, force_terminal=True, width=120)
        display = PipelineProgressDisplay(
            console=console,
            env_name="test",
            refresh_per_second=4,
        )
        display.start()
        display.stop()
        display.stop()  # Should not raise


class TestElapsedTimeTracking:
    """Tests for elapsed time tracking."""

    def test_start_time_set_on_phase_transition(self) -> None:
        """Start time should be recorded when leaving WAITING phase."""
        display = PipelineProgressDisplay(
            console=Console(file=StringIO()),
            env_name="test",
            refresh_per_second=4,
        )
        display.register_task("pkg", "1.0")
        state = display._states["pkg"]
        assert state.start_time is None

        display.on_progress("pkg", TaskPhase.DOWNLOADING, 0, 100, "Start")
        assert state.start_time is not None

    def test_elapsed_accumulates(self) -> None:
        """Elapsed should grow as time passes after starting."""
        display = PipelineProgressDisplay(
            console=Console(file=StringIO()),
            env_name="test",
            refresh_per_second=4,
        )
        display.register_task("pkg", "1.0")
        display.on_progress("pkg", TaskPhase.DOWNLOADING, 0, 100, "Start")
        snap1 = display.get_snapshot()
        elapsed1 = snap1[0]["elapsed"]
        assert elapsed1 >= 0

    def test_elapsed_frozen_on_done(self) -> None:
        """Elapsed should be recorded when task transitions to DONE."""
        display = PipelineProgressDisplay(
            console=Console(file=StringIO()),
            env_name="test",
            refresh_per_second=4,
        )
        display.register_task("pkg", "1.0")
        display.on_progress("pkg", TaskPhase.DOWNLOADING, 0, 100, "Start")
        display.on_progress("pkg", TaskPhase.DONE, 1, 1, "Complete")
        snap = display.get_snapshot()
        assert snap[0]["elapsed"] >= 0


class TestHeaderAndDisplay:
    """Tests for header, footer as separate renderables, and full display composition."""

    def _make_display(self, env_name: str) -> PipelineProgressDisplay:
        return PipelineProgressDisplay(
            console=Console(file=StringIO()),
            env_name=env_name,
            refresh_per_second=4,
        )

    def test_header_includes_env_name(self) -> None:
        """The full display should include 'Installing dependencies for env:X...'."""
        display = self._make_display("uno")
        display.register_task("pkg", "1.0")

        output = StringIO()
        console = Console(file=output, force_terminal=True, width=120)
        display._console = console
        group = display._render_display()
        console.print(group)
        text = output.getvalue()
        assert "Installing dependencies for env:uno..." in text

    def test_display_contains_all_sections(self) -> None:
        """Full display should contain header, tasks, and footer."""
        display = self._make_display("esp32")
        display.register_task("toolchain", "1.0")
        display.register_task("framework", "2.0")
        display.on_progress("toolchain", TaskPhase.DOWNLOADING, 50, 100, "1.2 MB/s")
        display.on_progress("framework", TaskPhase.DONE, 1, 1, "Done")

        output = StringIO()
        console = Console(file=output, force_terminal=True, width=120)
        display._console = console
        group = display._render_display()
        console.print(group)
        text = output.getvalue()

        # Header
        assert "Installing dependencies for env:esp32..." in text
        # Tasks
        assert "toolchain" in text
        assert "framework" in text
        # Footer
        assert "2 packages" in text
        assert "1 done" in text

    def test_spinner_character_in_installing(self) -> None:
        """INSTALLING status should contain a braille spinner character."""
        display = self._make_display("test")
        display.register_task("pkg", "1.0")
        display.on_progress("pkg", TaskPhase.INSTALLING, 1, 3, "Verifying...")
        state = display._states["pkg"]
        text = display._format_status(state)
        plain = text.plain
        # Check that spinner char is one of the braille set
        braille_chars = set("\u280b\u2819\u2839\u2838\u283c\u2834\u2826\u2827\u2807\u280f")
        assert any(c in plain for c in braille_chars), f"No spinner char found in: {plain!r}"

    def test_done_unicode_checkmark(self) -> None:
        """DONE status should use Unicode checkmark character."""
        display = self._make_display("test")
        display.register_task("pkg", "1.0")
        display.on_progress("pkg", TaskPhase.DOWNLOADING, 0, 100, "Start")
        display.on_progress("pkg", TaskPhase.DONE, 1, 1, "Done")
        state = display._states["pkg"]
        text = display._format_status(state)
        assert "\u2713" in text.plain

    def test_failed_unicode_cross(self) -> None:
        """FAILED status should use Unicode cross character."""
        display = self._make_display("test")
        display.register_task("pkg", "1.0")
        display.on_progress("pkg", TaskPhase.FAILED, 0, 0, "Network error")
        state = display._states["pkg"]
        text = display._format_status(state)
        assert "\u2717" in text.plain
        assert "Network error" in text.plain

    def test_footer_is_text_renderable(self) -> None:
        """Footer should be a Rich Text object with correct content."""
        from rich.text import Text as RichText

        display = self._make_display("test")
        display.register_task("a", "1.0")
        footer = display._render_footer()
        assert isinstance(footer, RichText)
        assert "1 packages" in footer.plain


class TestVerboseMode:
    """Tests for verbose mode showing URLs and destination paths."""

    def test_verbose_shows_url_when_active(self) -> None:
        """Verbose mode should show URL sub-line for active tasks."""
        output = StringIO()
        console = Console(file=output, force_terminal=True, width=120)
        display = PipelineProgressDisplay(
            console=console,
            env_name="test",
            refresh_per_second=4,
            verbose=True,
        )
        display.register_task("toolchain", "1.0", url="https://example.com/toolchain.tar.gz", dest_path="/tmp/toolchain")
        display.on_progress("toolchain", TaskPhase.DOWNLOADING, 50, 100, "1.2 MB/s")

        group = display._render_display()
        console.print(group)
        text = output.getvalue()
        assert "https://example.com/toolchain.tar.gz" in text

    def test_verbose_shows_dest_path(self) -> None:
        """Verbose mode should show destination path sub-line."""
        output = StringIO()
        console = Console(file=output, force_terminal=True, width=120)
        display = PipelineProgressDisplay(
            console=console,
            env_name="test",
            refresh_per_second=4,
            verbose=True,
        )
        display.register_task("pkg", "1.0", url="https://example.com/pkg.tar.gz", dest_path="/cache/pkg/1.0")
        display.on_progress("pkg", TaskPhase.INSTALLING, 1, 3, "Verifying...")

        group = display._render_display()
        console.print(group)
        text = output.getvalue()
        assert "/cache/pkg/1.0" in text

    def test_non_verbose_hides_url(self) -> None:
        """Non-verbose mode should NOT show URL sub-lines."""
        output = StringIO()
        console = Console(file=output, force_terminal=True, width=120)
        display = PipelineProgressDisplay(
            console=console,
            env_name="test",
            refresh_per_second=4,
            verbose=False,
        )
        display.register_task("pkg", "1.0", url="https://example.com/pkg.tar.gz", dest_path="/cache/pkg/1.0")
        display.on_progress("pkg", TaskPhase.DOWNLOADING, 50, 100, "1.2 MB/s")

        group = display._render_display()
        console.print(group)
        text = output.getvalue()
        assert "https://example.com/pkg.tar.gz" not in text

    def test_verbose_hides_details_for_waiting(self) -> None:
        """Verbose mode should not show URL/path for WAITING tasks."""
        output = StringIO()
        console = Console(file=output, force_terminal=True, width=120)
        display = PipelineProgressDisplay(
            console=console,
            env_name="test",
            refresh_per_second=4,
            verbose=True,
        )
        display.register_task("pkg", "1.0", url="https://example.com/pkg.tar.gz", dest_path="/cache/pkg/1.0")
        # Task stays in WAITING - don't call on_progress

        group = display._render_display()
        console.print(group)
        text = output.getvalue()
        assert "https://example.com/pkg.tar.gz" not in text


class TestNonTTYFallback:
    """Tests for non-TTY (piped/CI) output behavior."""

    def test_verbose_callback_prints_phase_transitions(self) -> None:
        """_VerboseCallback should print text-based progress for non-TTY mode."""
        from fbuild.packages.pipeline import _VerboseCallback

        callback = _VerboseCallback()
        # Capture stdout
        import contextlib
        import io

        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            callback.on_progress("toolchain", TaskPhase.DOWNLOADING, 50, 100, "50% complete")
            callback.on_progress("toolchain", TaskPhase.DONE, 1, 1, "Done in 2.1s")
        output = buf.getvalue()
        assert "toolchain" in output
        assert "50%" in output
        assert "Done" in output

    def test_verbose_callback_shows_failed_on_stderr(self) -> None:
        """_VerboseCallback should print failures to stderr."""
        from fbuild.packages.pipeline import _VerboseCallback

        callback = _VerboseCallback()
        import contextlib
        import io

        buf = io.StringIO()
        with contextlib.redirect_stderr(buf):
            callback.on_progress("pkg", TaskPhase.FAILED, 0, 0, "Network timeout")
        output = buf.getvalue()
        assert "pkg" in output
        assert "Failed" in output
        assert "Network timeout" in output
