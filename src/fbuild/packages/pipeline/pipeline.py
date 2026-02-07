"""Pipeline orchestrator connecting scheduler + pools for parallel package installation.

Coordinates the three-phase pipeline (download -> unpack -> install) by:
1. Using DependencyScheduler to resolve task ordering
2. Submitting ready tasks to the appropriate pool based on their phase
3. Tracking futures and transitioning tasks through phases on completion
4. Handling errors by failing dependent tasks
5. Supporting Ctrl-C cancellation with graceful pool shutdown and cleanup
"""

import logging
import threading
import time
from concurrent.futures import Future
from pathlib import Path
from typing import Any

from .callbacks import ProgressCallback
from .models import PackageTask, PipelineResult, TaskPhase
from .pools import DownloadPool, InstallPool, UnpackPool
from .scheduler import DependencyScheduler

logger = logging.getLogger(__name__)


class PipelineCancelledError(Exception):
    """Raised when the pipeline is cancelled via Ctrl-C or explicit cancellation."""

    pass


class ParallelPipeline:
    """Orchestrates parallel package installation through download -> unpack -> install phases.

    Connects the DependencyScheduler with three static thread pools (DownloadPool,
    UnpackPool, InstallPool) to process packages in parallel while respecting
    dependency ordering.

    Args:
        download_workers: Number of concurrent download threads.
        unpack_workers: Number of concurrent unpack threads.
        install_workers: Number of concurrent install threads.
    """

    def __init__(self, download_workers: int, unpack_workers: int, install_workers: int) -> None:
        self._download_workers = download_workers
        self._unpack_workers = unpack_workers
        self._install_workers = install_workers
        self._cancelled = False
        self._lock = threading.Lock()

    def run(self, tasks: list[PackageTask], callback: ProgressCallback) -> PipelineResult:
        """Execute the pipeline on the given tasks.

        Processes tasks through download -> unpack -> install phases, respecting
        dependency ordering from the scheduler. Returns when all tasks are in a
        terminal state (DONE or FAILED).

        Args:
            tasks: List of package tasks to process.
            callback: Progress callback for reporting updates.

        Returns:
            PipelineResult with final task states and timing.

        Raises:
            PipelineCancelledError: If the pipeline is cancelled via cancel().
        """
        start_time = time.monotonic()
        self._cancelled = False

        if not tasks:
            return PipelineResult(tasks=[], total_elapsed=0.0, success=True)

        # Build scheduler from tasks
        scheduler = DependencyScheduler()
        for task in tasks:
            scheduler.add_task(task)
        scheduler.validate()

        # Track active futures: future -> (task_name, phase)
        active_futures: dict[Future[Any], tuple[str, TaskPhase]] = {}

        with (
            DownloadPool(max_workers=self._download_workers) as download_pool,
            UnpackPool(max_workers=self._unpack_workers) as unpack_pool,
            InstallPool(max_workers=self._install_workers) as install_pool,
        ):
            try:
                while not scheduler.all_done():
                    if self._is_cancelled():
                        self._cancel_active_futures(active_futures)
                        self._fail_remaining_tasks(scheduler, "Pipeline cancelled")
                        self._cleanup_partial_downloads(scheduler)
                        raise PipelineCancelledError("Pipeline was cancelled")

                    # Fail tasks blocked by failed dependencies
                    self._fail_blocked_tasks(scheduler, callback)

                    # Submit ready tasks
                    ready = scheduler.get_ready_tasks()
                    for task in ready:
                        if self._is_cancelled():
                            break
                        self._submit_task(
                            task,
                            scheduler,
                            download_pool,
                            callback,
                            active_futures,
                        )

                    # Check completed futures and transition tasks
                    self._process_completed_futures(
                        active_futures,
                        scheduler,
                        unpack_pool,
                        install_pool,
                        callback,
                    )

                    # Brief sleep to avoid busy-waiting
                    time.sleep(0.05)

            except KeyboardInterrupt:
                self._cancel_active_futures(active_futures)
                self._fail_remaining_tasks(scheduler, "Interrupted by user")
                self._cleanup_partial_downloads(scheduler)
                raise

        total_elapsed = time.monotonic() - start_time
        all_tasks = scheduler.get_all_tasks()
        success = all(t.phase == TaskPhase.DONE for t in all_tasks)
        return PipelineResult(tasks=all_tasks, total_elapsed=total_elapsed, success=success)

    def cancel(self) -> None:
        """Request pipeline cancellation. Thread-safe."""
        with self._lock:
            self._cancelled = True

    def _is_cancelled(self) -> bool:
        """Check if cancellation has been requested."""
        with self._lock:
            return self._cancelled

    def _submit_task(
        self,
        task: PackageTask,
        scheduler: DependencyScheduler,
        download_pool: DownloadPool,
        callback: ProgressCallback,
        active_futures: dict[Future[Any], tuple[str, TaskPhase]],
    ) -> None:
        """Submit a WAITING task to the download pool to begin processing.

        Args:
            task: Task to submit (must be in WAITING phase).
            scheduler: Scheduler to update task phase.
            download_pool: Pool for downloading.
            callback: Progress callback.
            active_futures: Dict tracking active futures.
        """
        task.mark_started()
        scheduler.mark_phase(task.name, TaskPhase.DOWNLOADING)
        callback.on_progress(task.name, TaskPhase.DOWNLOADING, 0, 0, "Queued for download")
        future = download_pool.submit_download(task, callback)
        active_futures[future] = (task.name, TaskPhase.DOWNLOADING)

    def _process_completed_futures(
        self,
        active_futures: dict[Future[Any], tuple[str, TaskPhase]],
        scheduler: DependencyScheduler,
        unpack_pool: UnpackPool,
        install_pool: InstallPool,
        callback: ProgressCallback,
    ) -> None:
        """Check for completed futures and transition tasks to the next phase.

        Args:
            active_futures: Dict of active futures to check.
            scheduler: Scheduler for phase updates.
            unpack_pool: Pool for unpacking.
            install_pool: Pool for installing.
            callback: Progress callback.
        """
        completed = [f for f in active_futures if f.done()]
        for future in completed:
            task_name, phase = active_futures.pop(future)
            task = scheduler.get_task(task_name)

            try:
                result = future.result()
                self._transition_task(
                    task,
                    result,
                    phase,
                    scheduler,
                    unpack_pool,
                    install_pool,
                    callback,
                    active_futures,
                )
            except KeyboardInterrupt:
                raise
            except Exception as e:
                task.fail(str(e))
                scheduler.mark_phase(task_name, TaskPhase.FAILED)
                callback.on_progress(task_name, TaskPhase.FAILED, 0, 0, str(e))

    def _transition_task(
        self,
        task: PackageTask,
        result: Any,
        completed_phase: TaskPhase,
        scheduler: DependencyScheduler,
        unpack_pool: UnpackPool,
        install_pool: InstallPool,
        callback: ProgressCallback,
        active_futures: dict[Future[Any], tuple[str, TaskPhase]],
    ) -> None:
        """Transition a task to its next phase after successful completion.

        Args:
            task: The completed task.
            result: Result from the completed future (Path for download/unpack).
            completed_phase: The phase that just completed.
            scheduler: Scheduler for phase updates.
            unpack_pool: Pool for unpacking.
            install_pool: Pool for installing.
            callback: Progress callback.
            active_futures: Dict tracking active futures.
        """
        if completed_phase == TaskPhase.DOWNLOADING:
            # Download complete -> start unpacking
            archive_path = Path(result) if not isinstance(result, Path) else result
            task.archive_path = str(archive_path)
            scheduler.mark_phase(task.name, TaskPhase.UNPACKING)
            callback.on_progress(task.name, TaskPhase.UNPACKING, 0, 0, "Queued for extraction")
            future = unpack_pool.submit_unpack(task, archive_path, callback)
            active_futures[future] = (task.name, TaskPhase.UNPACKING)

        elif completed_phase == TaskPhase.UNPACKING:
            # Unpack complete -> start installing
            extracted_path = Path(result) if not isinstance(result, Path) else result
            task.extracted_path = str(extracted_path)
            scheduler.mark_phase(task.name, TaskPhase.INSTALLING)
            callback.on_progress(task.name, TaskPhase.INSTALLING, 0, 0, "Queued for installation")
            future = install_pool.submit_install(task, extracted_path, callback)
            active_futures[future] = (task.name, TaskPhase.INSTALLING)

        elif completed_phase == TaskPhase.INSTALLING:
            # Install complete -> done
            task.update_elapsed()
            scheduler.mark_phase(task.name, TaskPhase.DONE)
            callback.on_progress(task.name, TaskPhase.DONE, 1, 1, f"Done in {task.elapsed:.1f}s")

    def _fail_blocked_tasks(self, scheduler: DependencyScheduler, callback: ProgressCallback) -> None:
        """Mark tasks as FAILED if they are blocked by a failed dependency.

        Args:
            scheduler: Scheduler to query for blocked tasks.
            callback: Progress callback.
        """
        blocked = scheduler.get_blocked_tasks()
        for task in blocked:
            # Find which dependency failed
            failed_dep = ""
            for dep_name in task.dependencies:
                dep_task = scheduler.get_task(dep_name)
                if dep_task.phase == TaskPhase.FAILED:
                    failed_dep = dep_name
                    break

            error_msg = f"Dependency '{failed_dep}' failed" if failed_dep else "Blocked by failed dependency"
            task.fail(error_msg)
            scheduler.mark_phase(task.name, TaskPhase.FAILED)
            callback.on_progress(task.name, TaskPhase.FAILED, 0, 0, error_msg)

    def _cancel_active_futures(self, active_futures: dict[Future[Any], tuple[str, TaskPhase]]) -> None:
        """Cancel all active futures.

        Args:
            active_futures: Dict of futures to cancel.
        """
        for future in active_futures:
            future.cancel()

    def _fail_remaining_tasks(self, scheduler: DependencyScheduler, reason: str) -> None:
        """Mark all non-terminal tasks as FAILED.

        Args:
            scheduler: Scheduler containing all tasks.
            reason: Failure reason message.
        """
        for task in scheduler.get_all_tasks():
            if task.phase not in (TaskPhase.DONE, TaskPhase.FAILED):
                task.fail(reason)
                scheduler.mark_phase(task.name, TaskPhase.FAILED)

    def _cleanup_partial_downloads(self, scheduler: DependencyScheduler) -> None:
        """Remove partial download temp files left by interrupted downloads.

        Looks for .download temp files next to task dest_path locations
        and removes them to avoid leaving stale partial files on disk.

        Args:
            scheduler: Scheduler containing all tasks.
        """
        for task in scheduler.get_all_tasks():
            if task.phase == TaskPhase.FAILED:
                try:
                    dest_path = Path(task.dest_path)
                    # Clean up .download temp files in parent directory
                    parent = dest_path.parent
                    if parent.exists():
                        for temp_file in parent.glob("*.download"):
                            try:
                                temp_file.unlink()
                                logger.debug("Cleaned up partial download: %s", temp_file)
                            except (PermissionError, OSError):
                                pass
                    # Clean up temp extraction directories
                    if parent.exists():
                        for temp_dir in parent.glob("temp_extract_*"):
                            try:
                                import shutil

                                shutil.rmtree(temp_dir, ignore_errors=True)
                                logger.debug("Cleaned up temp extraction dir: %s", temp_dir)
                            except (PermissionError, OSError):
                                pass
                except KeyboardInterrupt:
                    raise
                except Exception:
                    pass  # Best-effort cleanup
