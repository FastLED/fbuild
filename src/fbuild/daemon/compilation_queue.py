"""
Compilation Job Queue - Parallel compilation with worker pool.

This module provides a background compilation queue that enables parallel
compilation of source files using a worker thread pool. It replaces direct
synchronous subprocess.run() calls with asynchronous job submission.
"""

import logging
import multiprocessing
import subprocess
import time
from dataclasses import dataclass
from enum import Enum
from pathlib import Path
from queue import Empty, Queue
from threading import Lock, Thread
from typing import Callable, Optional

from ..interrupt_utils import handle_keyboard_interrupt_properly


class JobState(Enum):
    """State of a compilation job."""

    PENDING = "pending"
    RUNNING = "running"
    COMPLETED = "completed"
    FAILED = "failed"
    CANCELLED = "cancelled"


@dataclass
class CompilationJob:
    """Single compilation job."""

    job_id: str
    source_path: Path
    output_path: Path
    compiler_cmd: list[str]  # Full command including compiler path
    response_file: Optional[Path] = None  # Response file for includes
    state: JobState = JobState.PENDING
    result_code: Optional[int] = None
    stdout: str = ""
    stderr: str = ""
    start_time: Optional[float] = None
    end_time: Optional[float] = None

    def duration(self) -> Optional[float]:
        """Get job duration in seconds."""
        if self.start_time and self.end_time:
            return self.end_time - self.start_time
        return None


class CompilationJobQueue:
    """Background compilation queue with worker pool."""

    def __init__(self, num_workers: Optional[int] = None):
        """Initialize compilation queue.

        Args:
            num_workers: Number of worker threads (default: CPU count)
        """
        self.num_workers = num_workers or multiprocessing.cpu_count()
        self.job_queue: Queue[CompilationJob] = Queue()
        self.jobs: dict[str, CompilationJob] = {}
        self.jobs_lock = Lock()
        self.workers: list[Thread] = []
        self.running = False
        self.progress_callback: Optional[Callable[[CompilationJob], None]] = None

        logging.info(f"CompilationJobQueue initialized with {self.num_workers} workers")

    def start(self) -> None:
        """Start worker threads."""
        if self.running:
            logging.warning("CompilationJobQueue already running")
            return

        self.running = True
        for i in range(self.num_workers):
            worker_name = f"CompilationWorker-{i}"
            worker = Thread(target=self._worker_loop, name=worker_name, daemon=True)
            worker.start()
            self.workers.append(worker)

        logging.info(f"Started {self.num_workers} compilation workers")

    def submit_job(self, job: CompilationJob) -> str:
        """Submit compilation job to queue.

        Args:
            job: Compilation job to submit

        Returns:
            Job ID
        """
        with self.jobs_lock:
            self.jobs[job.job_id] = job

        self.job_queue.put(job)
        current_depth = self.job_queue.qsize()

        if current_depth > self.num_workers * 2:
            logging.warning(f"Queue depth high: {current_depth} pending jobs")

        return job.job_id

    def _worker_loop(self) -> None:
        """Worker thread main loop."""
        import threading

        thread_name = threading.current_thread().name

        while self.running:
            try:
                job = self.job_queue.get(timeout=1.0)
                self._execute_job(job)
            except Empty:
                continue
            except KeyboardInterrupt as ke:
                handle_keyboard_interrupt_properly(ke)
            except Exception as e:
                logging.error(f"Worker {thread_name} error: {e}", exc_info=True)

    def _execute_job(self, job: CompilationJob) -> None:
        """Execute single compilation job.

        Args:
            job: Compilation job to execute
        """
        with self.jobs_lock:
            job.state = JobState.RUNNING
            job.start_time = time.time()

        # Notify progress callback
        if self.progress_callback:
            try:
                self.progress_callback(job)
            except KeyboardInterrupt as ke:
                handle_keyboard_interrupt_properly(ke)
            except Exception as e:
                logging.error(f"Progress callback error: {e}", exc_info=True)

        try:
            # Execute compiler subprocess
            result = subprocess.run(job.compiler_cmd, capture_output=True, text=True, timeout=60)

            with self.jobs_lock:
                job.result_code = result.returncode
                job.stdout = result.stdout
                job.stderr = result.stderr
                job.end_time = time.time()

                if result.returncode == 0:
                    job.state = JobState.COMPLETED
                else:
                    job.state = JobState.FAILED
                    logging.error(f"Job {job.job_id} failed with exit code {result.returncode}: {job.source_path.name}")

        except subprocess.TimeoutExpired:
            with self.jobs_lock:
                job.state = JobState.FAILED
                job.stderr = "Compilation timeout (60s exceeded)"
                job.end_time = time.time()
            logging.error(f"Job {job.job_id} timed out after 60s: {job.source_path.name}")

        except KeyboardInterrupt as ke:
            handle_keyboard_interrupt_properly(ke)

        except Exception as e:
            with self.jobs_lock:
                job.state = JobState.FAILED
                job.stderr = str(e)
                job.end_time = time.time()
            logging.error(f"Job {job.job_id} exception: {e}", exc_info=True)

        # Notify progress callback
        if self.progress_callback:
            try:
                self.progress_callback(job)
            except KeyboardInterrupt as ke:
                handle_keyboard_interrupt_properly(ke)
            except Exception as e:
                logging.error(f"Progress callback error: {e}", exc_info=True)

    def get_job_status(self, job_id: str) -> Optional[CompilationJob]:
        """Get status of a specific job.

        Args:
            job_id: Job ID to query

        Returns:
            Compilation job or None if not found
        """
        with self.jobs_lock:
            return self.jobs.get(job_id)

    def wait_for_completion(self, job_ids: list[str], timeout: Optional[float] = None) -> bool:
        """Wait for all specified jobs to complete.

        Args:
            job_ids: List of job IDs to wait for
            timeout: Maximum time to wait in seconds (None = infinite)

        Returns:
            True if all jobs completed successfully, False otherwise
        """
        start_time = time.time()

        while True:
            with self.jobs_lock:
                all_done = all(self.jobs[jid].state in (JobState.COMPLETED, JobState.FAILED, JobState.CANCELLED) for jid in job_ids if jid in self.jobs)
                if all_done:
                    success = all(self.jobs[jid].state == JobState.COMPLETED for jid in job_ids if jid in self.jobs)
                    completed_count = sum(1 for jid in job_ids if jid in self.jobs and self.jobs[jid].state == JobState.COMPLETED)
                    failed_count = sum(1 for jid in job_ids if jid in self.jobs and self.jobs[jid].state == JobState.FAILED)
                    if failed_count > 0:
                        logging.warning(f"Compilation completed: {completed_count} succeeded, {failed_count} failed")
                    return success

            # Check timeout
            if timeout and (time.time() - start_time) > timeout:
                with self.jobs_lock:
                    remaining = sum(1 for jid in job_ids if jid in self.jobs and self.jobs[jid].state == JobState.PENDING)
                logging.warning(f"wait_for_completion timed out after {timeout}s ({remaining} jobs still pending)")
                return False

            time.sleep(0.1)

    def cancel_jobs(self, job_ids: list[str]) -> None:
        """Cancel pending jobs (cannot cancel running jobs).

        Args:
            job_ids: List of job IDs to cancel
        """
        with self.jobs_lock:
            cancelled_count = 0
            for jid in job_ids:
                if jid in self.jobs and self.jobs[jid].state == JobState.PENDING:
                    self.jobs[jid].state = JobState.CANCELLED
                    cancelled_count += 1

            if cancelled_count > 0:
                logging.info(f"Cancelled {cancelled_count} pending jobs")

    def get_statistics(self) -> dict[str, int]:
        """Get queue statistics.

        Returns:
            Dictionary with job counts by state
        """
        with self.jobs_lock:
            stats = {
                "total_jobs": len(self.jobs),
                "pending": sum(1 for j in self.jobs.values() if j.state == JobState.PENDING),
                "running": sum(1 for j in self.jobs.values() if j.state == JobState.RUNNING),
                "completed": sum(1 for j in self.jobs.values() if j.state == JobState.COMPLETED),
                "failed": sum(1 for j in self.jobs.values() if j.state == JobState.FAILED),
                "cancelled": sum(1 for j in self.jobs.values() if j.state == JobState.CANCELLED),
            }

        return stats

    def get_failed_jobs(self) -> list[CompilationJob]:
        """Get all failed jobs.

        Returns:
            List of failed compilation jobs
        """
        with self.jobs_lock:
            return [j for j in self.jobs.values() if j.state == JobState.FAILED]

    def clear_jobs(self) -> None:
        """Clear all completed/failed/cancelled jobs from registry."""
        with self.jobs_lock:
            to_remove = [jid for jid, job in self.jobs.items() if job.state in (JobState.COMPLETED, JobState.FAILED, JobState.CANCELLED)]

            for jid in to_remove:
                del self.jobs[jid]

    def shutdown(self) -> None:
        """Shutdown worker pool."""
        logging.info("Shutting down CompilationJobQueue")
        self.running = False

        for worker in self.workers:
            worker.join(timeout=2.0)
            if worker.is_alive():
                logging.warning(f"Worker {worker.name} did not finish within timeout")

        self.workers.clear()
