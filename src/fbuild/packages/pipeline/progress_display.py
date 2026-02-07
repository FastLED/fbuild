"""Rich-based Docker pull-style TUI progress display for the parallel package pipeline.

Renders a live-updating display showing all packages simultaneously with their
current phase, progress bars, spinners, and status text. Each package gets a
single line that transitions through phases:

    Waiting -> Downloading [=========>     ] 62% -> Unpacking [=====>  ] 85%
    -> Installing (spinner) Verifying... -> Done (checkmark) 3.2s

Thread-safe: multiple pool worker threads can call on_progress() concurrently
while the display renders in the main thread.
"""

import threading
import time
from typing import Any

from rich.console import Console, Group
from rich.live import Live
from rich.table import Table
from rich.text import Text

from .models import TaskPhase

# Braille spinner frames for the INSTALLING phase animation
_SPINNER_FRAMES = ("⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏")


class _TaskDisplayState:
    """Internal state for a single task's display line.

    Attributes:
        name: Package name.
        version: Package version string.
        phase: Current pipeline phase.
        progress: Current progress value (bytes, files, steps).
        total: Total expected value.
        detail: Human-readable status text.
        elapsed: Elapsed time in seconds.
        start_time: Monotonic timestamp when task entered non-WAITING phase.
    """

    __slots__ = ("name", "version", "phase", "progress", "total", "detail", "elapsed", "start_time")

    def __init__(self, name: str, version: str) -> None:
        self.name = name
        self.version = version
        self.phase = TaskPhase.WAITING
        self.progress: float = 0.0
        self.total: float = 0.0
        self.detail: str = ""
        self.elapsed: float = 0.0
        self.start_time: float | None = None


class PipelineProgressDisplay:
    """Docker pull-style TUI progress display using Rich.

    Implements ProgressCallback to receive real-time updates from pipeline
    pools and renders a live-updating table showing all packages with their
    current phase, progress bars, and status text.

    Args:
        console: Rich Console instance for rendering. If None, creates a new one.
        env_name: Environment name for the header line (e.g. "uno").
        refresh_per_second: Display refresh rate (default 10).
    """

    def __init__(self, console: Console | None, env_name: str, refresh_per_second: int, verbose: bool = False) -> None:
        self._console = console if console is not None else Console()
        self._env_name = env_name
        self._refresh_per_second = refresh_per_second
        self._verbose = verbose
        self._states: dict[str, _TaskDisplayState] = {}
        self._task_urls: dict[str, str] = {}
        self._task_dest_paths: dict[str, str] = {}
        self._lock = threading.Lock()
        self._live: Live | None = None
        self._task_order: list[str] = []

    def register_task(self, name: str, version: str, url: str = "", dest_path: str = "") -> None:
        """Register a task for display before the pipeline starts.

        Args:
            name: Package name (e.g. "toolchain-atmelavr").
            version: Package version string (e.g. "3.1.0").
            url: Source URL for verbose mode display.
            dest_path: Destination path for verbose mode display.
        """
        with self._lock:
            if name not in self._states:
                self._states[name] = _TaskDisplayState(name, version)
                self._task_order.append(name)
                if url:
                    self._task_urls[name] = url
                if dest_path:
                    self._task_dest_paths[name] = dest_path

    def on_progress(self, task_name: str, phase: TaskPhase, progress: float, total: float, detail: str) -> None:
        """Update the display state for a task. Thread-safe.

        Called by pool worker threads to report progress. The display table
        is regenerated on the next refresh cycle.

        Args:
            task_name: Name of the package task.
            phase: Current pipeline phase.
            progress: Current progress value.
            total: Total expected value.
            detail: Human-readable status detail.
        """
        with self._lock:
            state = self._states.get(task_name)
            if state is None:
                state = _TaskDisplayState(task_name, "")
                self._states[task_name] = state
                self._task_order.append(task_name)

            # Track start time for elapsed calculation
            if state.phase == TaskPhase.WAITING and phase != TaskPhase.WAITING:
                state.start_time = time.monotonic()

            state.phase = phase
            state.progress = progress
            state.total = total
            state.detail = detail

            # Update elapsed for terminal states
            if phase in (TaskPhase.DONE, TaskPhase.FAILED) and state.start_time is not None:
                state.elapsed = time.monotonic() - state.start_time
            elif state.start_time is not None:
                state.elapsed = time.monotonic() - state.start_time

    def start(self) -> None:
        """Start the live display. Call before pipeline.run()."""
        self._live = Live(
            self._render_display(),
            console=self._console,
            refresh_per_second=self._refresh_per_second,
            transient=False,
        )
        self._live.start()

    def stop(self) -> None:
        """Stop the live display. Call after pipeline.run()."""
        if self._live is not None:
            # Final render with latest state
            self._live.update(self._render_display())
            self._live.stop()
            self._live = None

    def update(self) -> None:
        """Force a display refresh. Called from the main loop."""
        if self._live is not None:
            self._live.update(self._render_display())

    def _render_display(self) -> Group:
        """Build the Rich Group containing header, table, and footer.

        Returns:
            A Rich Group with header text, task table, and footer summary.
        """
        header = Text(f"\nInstalling dependencies for env:{self._env_name}...\n", style="bold")
        table = self._render_table()
        footer = self._render_footer()
        return Group(header, table, footer)

    def _render_table(self) -> Table:
        """Build the Rich Table representing the current display state.

        Returns:
            A Rich Table with one row per task.
        """
        table = Table(
            show_header=False,
            show_edge=False,
            show_lines=False,
            box=None,
            padding=(0, 1),
            expand=False,
        )

        # Columns: name+version, phase, progress/status
        table.add_column("Package", style="bold", no_wrap=True, min_width=28)
        table.add_column("Phase", no_wrap=True, min_width=14)
        table.add_column("Status", no_wrap=True, min_width=40)

        with self._lock:
            for name in self._task_order:
                state = self._states.get(name)
                if state is None:
                    continue

                name_text = self._format_name(state)
                phase_text = self._format_phase(state)
                status_text = self._format_status(state)
                table.add_row(name_text, phase_text, status_text)

                # Verbose mode: show URL and dest path as indented sub-lines
                if self._verbose and state.phase != TaskPhase.WAITING:
                    url = self._task_urls.get(name, "")
                    dest = self._task_dest_paths.get(name, "")
                    if url:
                        table.add_row(Text(f"  \u2514 {url}", style="dim"), Text(""), Text(""))
                    if dest:
                        table.add_row(Text(f"  \u2514 {dest}", style="dim"), Text(""), Text(""))

        return table

    def _render_footer(self) -> Text:
        """Build the footer summary text.

        Returns:
            Rich Text with package counts summary.
        """
        with self._lock:
            total = len(self._states)
            done_count = sum(1 for s in self._states.values() if s.phase == TaskPhase.DONE)
            failed_count = sum(1 for s in self._states.values() if s.phase == TaskPhase.FAILED)
            active_count = sum(1 for s in self._states.values() if s.phase in (TaskPhase.DOWNLOADING, TaskPhase.UNPACKING, TaskPhase.INSTALLING))

        footer_parts = [f"{total} packages"]
        if active_count > 0:
            footer_parts.append(f"{active_count} active")
        if done_count > 0:
            footer_parts.append(f"{done_count} done")
        if failed_count > 0:
            footer_parts.append(f"{failed_count} failed")

        return Text(f"\n  {', '.join(footer_parts)}", style="dim")

    def _format_name(self, state: _TaskDisplayState) -> Text:
        """Format the package name + version column.

        Args:
            state: Task display state.

        Returns:
            Rich Text with styled name and version.
        """
        version_str = f" {state.version}" if state.version else ""
        if state.phase == TaskPhase.DONE:
            return Text(f"{state.name}{version_str}", style="green")
        elif state.phase == TaskPhase.FAILED:
            return Text(f"{state.name}{version_str}", style="red")
        elif state.phase == TaskPhase.WAITING:
            return Text(f"{state.name}{version_str}", style="dim")
        else:
            return Text(f"{state.name}{version_str}", style="bold cyan")

    def _format_phase(self, state: _TaskDisplayState) -> Text:
        """Format the phase column with appropriate styling.

        Args:
            state: Task display state.

        Returns:
            Rich Text with styled phase label.
        """
        phase_labels = {
            TaskPhase.WAITING: ("Waiting", "dim"),
            TaskPhase.DOWNLOADING: ("Downloading", "blue"),
            TaskPhase.UNPACKING: ("Unpacking", "yellow"),
            TaskPhase.INSTALLING: ("Installing", "magenta"),
            TaskPhase.DONE: ("Done", "green"),
            TaskPhase.FAILED: ("Failed", "red bold"),
        }
        label, style = phase_labels.get(state.phase, ("Unknown", "dim"))
        return Text(label, style=style)

    def _format_status(self, state: _TaskDisplayState) -> Text:
        """Format the status column with progress bar, spinner, or result.

        Args:
            state: Task display state.

        Returns:
            Rich Text with appropriate status visualization.
        """
        if state.phase == TaskPhase.WAITING:
            return Text("")

        elif state.phase == TaskPhase.DOWNLOADING:
            return self._format_progress_bar(state)

        elif state.phase == TaskPhase.UNPACKING:
            return self._format_progress_bar(state)

        elif state.phase == TaskPhase.INSTALLING:
            # Animated spinner using braille characters
            spinner_idx = int(time.monotonic() * 8) % len(_SPINNER_FRAMES)
            spinner = _SPINNER_FRAMES[spinner_idx]
            detail = state.detail if state.detail else "Processing..."
            return Text(f"{spinner} {detail}", style="magenta")

        elif state.phase == TaskPhase.DONE:
            elapsed_str = f"{state.elapsed:.1f}s" if state.elapsed > 0 else ""
            return Text(f"\u2713 {elapsed_str}", style="green")

        elif state.phase == TaskPhase.FAILED:
            detail = state.detail if state.detail else "Error"
            return Text(f"\u2717 {detail}", style="red")

        return Text("")

    def _format_progress_bar(self, state: _TaskDisplayState) -> Text:
        """Format a text-based progress bar for download/unpack phases.

        Args:
            state: Task display state.

        Returns:
            Rich Text with a progress bar like [=========>     ] 62%
        """
        bar_width = 20

        if state.total > 0:
            pct = min(state.progress / state.total, 1.0)
        else:
            pct = 0.0

        filled = int(bar_width * pct)
        remaining = bar_width - filled

        if filled < bar_width and filled > 0:
            bar = "=" * (filled - 1) + ">" + " " * remaining
        elif filled == bar_width:
            bar = "=" * bar_width
        else:
            bar = " " * bar_width

        pct_str = f"{pct * 100:.0f}%"

        detail = ""
        if state.detail and state.phase == TaskPhase.DOWNLOADING:
            # Show transfer speed if available
            if "/s" in state.detail:
                detail = f"  {state.detail}"

        style = "blue" if state.phase == TaskPhase.DOWNLOADING else "yellow"
        return Text(f"[{bar}] {pct_str:>4}{detail}", style=style)

    def get_snapshot(self) -> list[dict[str, Any]]:
        """Get a snapshot of current display states for testing.

        Returns:
            List of dicts with task display state information.
        """
        with self._lock:
            result = []
            for name in self._task_order:
                state = self._states.get(name)
                if state is None:
                    continue
                result.append(
                    {
                        "name": state.name,
                        "version": state.version,
                        "phase": state.phase,
                        "progress": state.progress,
                        "total": state.total,
                        "detail": state.detail,
                        "elapsed": state.elapsed,
                    }
                )
            return result

    def __enter__(self) -> "PipelineProgressDisplay":
        self.start()
        return self

    def __exit__(self, exc_type: Any, exc_val: Any, exc_tb: Any) -> None:
        self.stop()
