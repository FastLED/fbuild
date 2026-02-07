"""Progress callback protocol for the parallel package pipeline.

Defines the callback interface used by download, unpack, and install pools
to report progress to the TUI display layer.
"""

from typing import Protocol, runtime_checkable

from .models import TaskPhase


@runtime_checkable
class ProgressCallback(Protocol):
    """Protocol for receiving progress updates from pipeline pools.

    Implementations receive real-time updates as packages move through
    download, unpack, and install phases. The TUI display layer implements
    this protocol to render live progress bars and status text.
    """

    def on_progress(self, task_name: str, phase: TaskPhase, progress: float, total: float, detail: str) -> None:
        """Called when a task makes progress within a phase.

        Args:
            task_name: Name of the package task (e.g. "toolchain-atmelavr").
            phase: Current pipeline phase.
            progress: Current progress value (e.g. bytes downloaded, files extracted).
            total: Total expected value (e.g. total bytes, total files). May be 0 if unknown.
            detail: Human-readable status detail (e.g. "2.1 MB/s", "Verifying binaries...").
        """
        ...


class NullCallback:
    """No-op callback implementation for testing and non-interactive use.

    Silently discards all progress updates. Useful when running the pipeline
    without a TUI (e.g. in tests or CI environments).
    """

    def on_progress(self, task_name: str, phase: TaskPhase, progress: float, total: float, detail: str) -> None:
        """Discard progress update."""
        pass
