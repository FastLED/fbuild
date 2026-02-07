"""Parallel package installation pipeline with Docker pull-style TUI.

This module provides a parallel pipeline for downloading, unpacking, and installing
packages using static thread pools and a Rich-based progress display.

Public API:
    ParallelInstaller: High-level installer that builds task graphs from platformio.ini
                       and runs the pipeline with optional TUI progress display.
    ParallelPipeline: Low-level pipeline orchestrator for custom task lists.
"""

import sys
from pathlib import Path

from .adapters import (
    TaskGraphError,
    build_avr_task_graph,
    build_task_graph,
    filter_uncached_tasks,
    is_task_cached,
)
from .callbacks import NullCallback, ProgressCallback
from .models import PackageTask, PipelineResult, TaskPhase
from .pipeline import ParallelPipeline, PipelineCancelledError
from .pools import DownloadPool, InstallPool, UnpackPool
from .progress_display import PipelineProgressDisplay


class ParallelInstaller:
    """High-level parallel package installer with Docker pull-style TUI.

    Builds a dependency graph from a platformio.ini project configuration,
    checks the cache for already-installed packages, and runs the remaining
    packages through the parallel pipeline with optional progress display.

    Args:
        download_workers: Number of concurrent download threads.
        unpack_workers: Number of concurrent unpack threads.
        install_workers: Number of concurrent install threads.
    """

    def __init__(
        self,
        download_workers: int,
        unpack_workers: int,
        install_workers: int,
    ) -> None:
        self._download_workers = download_workers
        self._unpack_workers = unpack_workers
        self._install_workers = install_workers

    def install_dependencies(
        self,
        project_path: Path,
        env_name: str,
        verbose: bool,
        use_tui: bool | None = None,
    ) -> PipelineResult:
        """Install all dependencies for a project environment.

        Builds a PackageTask dependency graph from platformio.ini, filters
        out already-cached packages, and runs the pipeline for remaining
        packages. Shows a Docker pull-style TUI if running in a terminal.

        Args:
            project_path: Path to the project directory containing platformio.ini.
            env_name: Environment name from platformio.ini (e.g. "uno").
            verbose: Whether to show verbose output.
            use_tui: Override TUI display. None = auto-detect (TTY check).
                     True = force TUI. False = disable TUI.

        Returns:
            PipelineResult with final task states and timing.

        Raises:
            TaskGraphError: If the project config is invalid or unsupported.
            PipelineCancelledError: If the pipeline is cancelled.
        """
        from fbuild.packages.cache import Cache

        cache = Cache(project_path)

        # Build task graph from platformio.ini
        all_tasks = build_task_graph(project_path, env_name, cache)

        if not all_tasks:
            return PipelineResult(tasks=[], total_elapsed=0.0, success=True)

        # Filter out cached packages
        cached_tasks, uncached_tasks = filter_uncached_tasks(all_tasks, cache)

        if not uncached_tasks:
            # All packages cached - nothing to do
            if verbose:
                for task in cached_tasks:
                    print(f"  {task.name} {task.version} - cached")
            return PipelineResult(
                tasks=all_tasks,
                total_elapsed=0.0,
                success=True,
            )

        # Remove dependencies on cached tasks from uncached tasks
        cached_names = {t.name for t in cached_tasks}
        for task in uncached_tasks:
            task.dependencies = [d for d in task.dependencies if d not in cached_names]

        # Determine whether to use TUI
        if use_tui is None:
            use_tui = _is_tty()

        # Create pipeline
        pipeline = ParallelPipeline(
            download_workers=self._download_workers,
            unpack_workers=self._unpack_workers,
            install_workers=self._install_workers,
        )

        if use_tui:
            # Run with Rich TUI progress display
            display = PipelineProgressDisplay(
                console=None,
                env_name=env_name,
                refresh_per_second=10,
                verbose=verbose,
            )

            # Register tasks for display (with verbose metadata)
            for task in uncached_tasks:
                display.register_task(task.name, task.version, url=task.url, dest_path=task.dest_path)

            with display:
                result = pipeline.run(uncached_tasks, display)
        else:
            # Run without TUI (CI, non-TTY, or explicit disable)
            callback = NullCallback() if not verbose else _VerboseCallback()
            result = pipeline.run(uncached_tasks, callback)

        # Merge cached tasks back into result
        merged_tasks = cached_tasks + result.tasks
        return PipelineResult(
            tasks=merged_tasks,
            total_elapsed=result.total_elapsed,
            success=result.success,
        )


class _VerboseCallback:
    """Simple text-based callback for non-TUI verbose mode."""

    def on_progress(
        self,
        task_name: str,
        phase: TaskPhase,
        progress: float,
        total: float,
        detail: str,
    ) -> None:
        """Print verbose progress updates."""
        phase_str = phase.value.capitalize()
        if phase == TaskPhase.DONE:
            print(f"  {task_name}: {phase_str} - {detail}")
        elif phase == TaskPhase.FAILED:
            print(f"  {task_name}: {phase_str} - {detail}", file=sys.stderr)
        elif total > 0:
            pct = int(progress / total * 100) if total > 0 else 0
            print(f"  {task_name}: {phase_str} {pct}% - {detail}")


def _is_tty() -> bool:
    """Check if stdout is a terminal (TTY).

    Returns:
        True if stdout is connected to a terminal.
    """
    try:
        return sys.stdout.isatty()
    except (AttributeError, ValueError):
        return False


__all__ = [
    "DownloadPool",
    "InstallPool",
    "NullCallback",
    "PackageTask",
    "ParallelInstaller",
    "ParallelPipeline",
    "PipelineCancelledError",
    "PipelineProgressDisplay",
    "PipelineResult",
    "ProgressCallback",
    "TaskGraphError",
    "TaskPhase",
    "UnpackPool",
    "build_avr_task_graph",
    "build_task_graph",
    "filter_uncached_tasks",
    "is_task_cached",
]
