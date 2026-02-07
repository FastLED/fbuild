"""Data models for the parallel package pipeline.

Defines the core dataclasses used throughout the pipeline:
- TaskPhase: Enum tracking which stage a package task is in
- PackageTask: Represents a single package to download/unpack/install
- PipelineResult: Aggregated result of running the full pipeline
"""

import time
from dataclasses import dataclass, field
from enum import Enum
from typing import Any


class TaskPhase(Enum):
    """Phase of a package task in the pipeline."""

    WAITING = "waiting"
    DOWNLOADING = "downloading"
    UNPACKING = "unpacking"
    INSTALLING = "installing"
    DONE = "done"
    FAILED = "failed"


@dataclass
class PackageTask:
    """A single package to be processed through the pipeline.

    Attributes:
        name: Human-readable package name (e.g. "toolchain-atmelavr")
        url: Download URL for the package archive
        version: Package version string
        dest_path: Final installation path
        dependencies: Names of tasks that must complete before this one starts
        phase: Current pipeline phase
        progress_pct: Progress percentage within current phase (0.0 to 100.0)
        status_text: Human-readable status detail (e.g. "Verifying binaries...")
        elapsed: Elapsed time in seconds since task started processing
        error_message: Error detail if phase is FAILED
        archive_path: Path to downloaded archive (set after download completes)
        extracted_path: Path to extracted contents (set after unpack completes)
        start_time: Timestamp when task started processing (None if not started)
        total_bytes: Total bytes to download (for progress display)
        downloaded_bytes: Bytes downloaded so far
    """

    name: str
    url: str
    version: str
    dest_path: str
    dependencies: list[str] = field(default_factory=list)
    phase: TaskPhase = TaskPhase.WAITING
    progress_pct: float = 0.0
    status_text: str = ""
    elapsed: float = 0.0
    error_message: str = ""
    archive_path: str = ""
    extracted_path: str = ""
    start_time: float | None = None
    total_bytes: int = 0
    downloaded_bytes: int = 0

    def mark_started(self) -> None:
        """Record the start time for elapsed time tracking."""
        self.start_time = time.monotonic()

    def update_elapsed(self) -> None:
        """Update elapsed time from start_time."""
        if self.start_time is not None:
            self.elapsed = time.monotonic() - self.start_time

    def fail(self, error: str) -> None:
        """Mark this task as failed with an error message."""
        self.phase = TaskPhase.FAILED
        self.error_message = error
        self.update_elapsed()

    def to_dict(self) -> dict[str, Any]:
        """Serialize to dictionary."""
        return {
            "name": self.name,
            "url": self.url,
            "version": self.version,
            "dest_path": self.dest_path,
            "dependencies": list(self.dependencies),
            "phase": self.phase.value,
            "progress_pct": self.progress_pct,
            "status_text": self.status_text,
            "elapsed": self.elapsed,
            "error_message": self.error_message,
            "archive_path": self.archive_path,
            "extracted_path": self.extracted_path,
            "total_bytes": self.total_bytes,
            "downloaded_bytes": self.downloaded_bytes,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "PackageTask":
        """Deserialize from dictionary."""
        return cls(
            name=data["name"],
            url=data["url"],
            version=data["version"],
            dest_path=data["dest_path"],
            dependencies=data.get("dependencies", []),
            phase=TaskPhase(data.get("phase", "waiting")),
            progress_pct=data.get("progress_pct", 0.0),
            status_text=data.get("status_text", ""),
            elapsed=data.get("elapsed", 0.0),
            error_message=data.get("error_message", ""),
            archive_path=data.get("archive_path", ""),
            extracted_path=data.get("extracted_path", ""),
            total_bytes=data.get("total_bytes", 0),
            downloaded_bytes=data.get("downloaded_bytes", 0),
        )


@dataclass
class PipelineResult:
    """Aggregated result of running the full pipeline.

    Attributes:
        tasks: Final state of all tasks after pipeline completes
        total_elapsed: Total wall-clock time in seconds
        success: True if all tasks completed successfully
    """

    tasks: list[PackageTask]
    total_elapsed: float
    success: bool

    @property
    def completed_count(self) -> int:
        """Number of tasks that completed successfully."""
        return sum(1 for t in self.tasks if t.phase == TaskPhase.DONE)

    @property
    def failed_count(self) -> int:
        """Number of tasks that failed."""
        return sum(1 for t in self.tasks if t.phase == TaskPhase.FAILED)

    @property
    def failed_tasks(self) -> list[PackageTask]:
        """List of tasks that failed."""
        return [t for t in self.tasks if t.phase == TaskPhase.FAILED]

    def to_dict(self) -> dict[str, Any]:
        """Serialize to dictionary."""
        return {
            "tasks": [t.to_dict() for t in self.tasks],
            "total_elapsed": self.total_elapsed,
            "success": self.success,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "PipelineResult":
        """Deserialize from dictionary."""
        return cls(
            tasks=[PackageTask.from_dict(t) for t in data["tasks"]],
            total_elapsed=data["total_elapsed"],
            success=data["success"],
        )
