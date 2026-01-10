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
        logging.debug(f"Initializing compilation queue (requested workers: {num_workers})")
        self.num_workers = num_workers or multiprocessing.cpu_count()
        logging.debug(f"Determined worker count: {self.num_workers} (CPU count: {multiprocessing.cpu_count()})")
        self.job_queue: Queue[CompilationJob] = Queue()
        logging.debug(f"Queue initial size: {self.job_queue.qsize()}")
        self.jobs: dict[str, CompilationJob] = {}
        self.jobs_lock = Lock()
        self.workers: list[Thread] = []
        self.running = False
        self.progress_callback: Optional[Callable[[CompilationJob], None]] = None
        logging.debug("Compilation queue data structures initialized")

        logging.info(f"CompilationJobQueue initialized with {self.num_workers} workers")

    def start(self) -> None:
        """Start worker threads."""
        logging.debug(f"Starting compilation worker pool (running: {self.running})")
        if self.running:
            logging.warning("CompilationJobQueue already running")
            return

        logging.debug(f"Initializing worker pool with {self.num_workers} threads")
        self.running = True
        for i in range(self.num_workers):
            worker_name = f"CompilationWorker-{i}"
            logging.debug(f"Creating worker thread {i+1}/{self.num_workers}: {worker_name}")
            worker = Thread(target=self._worker_loop, name=worker_name, daemon=True)
            worker.start()
            self.workers.append(worker)
            logging.debug(f"Worker thread {worker_name} started successfully")

        logging.debug(f"Worker threads: {[w.name for w in self.workers]}")
        logging.info(f"Started {self.num_workers} compilation workers")

    def submit_job(self, job: CompilationJob) -> str:
        """Submit compilation job to queue.

        Args:
            job: Compilation job to submit

        Returns:
            Job ID
        """
        logging.info(f"Submitting compilation job: {job.job_id}")
        logging.debug(f"Job details: source={job.source_path.name}, output={job.output_path.name}")
        logging.debug(f"Compile command: {' '.join(job.compiler_cmd[:3])}... ({len(job.compiler_cmd)} args)")
        logging.debug(f"Current queue depth before submit: {self.job_queue.qsize()}")

        with self.jobs_lock:
            self.jobs[job.job_id] = job
            logging.debug(f"Job {job.job_id} added to tracking dict")
            logging.debug(f"Active jobs count: {len(self.jobs)}")

        self.job_queue.put(job)
        current_depth = self.job_queue.qsize()
        logging.info(f"Job submitted successfully: {job.job_id} (queue depth: {current_depth})")

        if current_depth > self.num_workers * 2:
            logging.warning(f"Queue depth high: {current_depth} pending jobs")

        return job.job_id

    def _worker_loop(self) -> None:
        """Worker thread main loop."""
        import threading

        thread_name = threading.current_thread().name
        logging.debug(f"Worker {thread_name} starting")

        while self.running:
            try:
                logging.debug(f"Worker {thread_name} waiting for job")
                job = self.job_queue.get(timeout=1.0)
                logging.debug(f"Worker {thread_name} acquired job: {job.job_id}")
                self._execute_job(job)
                logging.debug(f"Worker {thread_name} job complete, returning to queue")
            except Empty:
                continue
            except KeyboardInterrupt as ke:
                handle_keyboard_interrupt_properly(ke)
            except Exception as e:
                logging.error(f"Worker {thread_name} error: {e}", exc_info=True)

        logging.info(f"Worker {thread_name} exiting (shutdown requested)")

    def _execute_job(self, job: CompilationJob) -> None:
        """Execute single compilation job.

        Args:
            job: Compilation job to execute
        """
        import threading

        thread_name = threading.current_thread().name

        logging.info(f"Worker {thread_name} executing job: {job.job_id}")
        logging.debug(f"Job source: {job.source_path}")
        logging.debug(f"Job output: {job.output_path}")

        with self.jobs_lock:
            job.state = JobState.RUNNING
            job.start_time = time.time()
            logging.debug(f"Job {job.job_id} state updated to RUNNING at {job.start_time}")

        # Notify progress callback
        if self.progress_callback:
            logging.debug(f"Calling progress callback for job {job.job_id} (start)")
            try:
                self.progress_callback(job)
            except KeyboardInterrupt as ke:
                handle_keyboard_interrupt_properly(ke)
            except Exception as e:
                logging.error(f"Progress callback error: {e}", exc_info=True)

        try:
            # Execute compiler subprocess
            logging.debug(f"Executing compilation command: {' '.join(job.compiler_cmd[:3])}...")
            logging.debug(f"Command length: {len(job.compiler_cmd)} arguments")
            result = subprocess.run(job.compiler_cmd, capture_output=True, text=True, timeout=60)

            with self.jobs_lock:
                job.result_code = result.returncode
                job.stdout = result.stdout
                job.stderr = result.stderr
                job.end_time = time.time()
                duration = job.duration()

                logging.debug(f"Compilation output: {len(result.stdout)} bytes stdout, {len(result.stderr)} bytes stderr")

                if result.returncode == 0:
                    job.state = JobState.COMPLETED
                    logging.info(f"Job {job.job_id} completed successfully (duration: {duration:.2f}s)")
                    logging.debug(f"Job {job.job_id} completed in {duration:.2f}s")
                else:
                    job.state = JobState.FAILED
                    logging.error(f"Job {job.job_id} failed with exit code {result.returncode}")
                    logging.warning(f"Job {job.job_id} failed with exit code {result.returncode}: {job.source_path.name}")
                    if result.stderr:
                        stderr_preview = result.stderr[:200].replace("\n", " ")
                        logging.debug(f"Job {job.job_id} stderr preview: {stderr_preview}")

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
            logging.debug(f"Calling progress callback for job {job.job_id} (complete)")
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
        logging.debug(f"Querying job status: {job_id}")
        with self.jobs_lock:
            job = self.jobs.get(job_id)
            if job:
                logging.debug(f"Job {job_id} status: {job.state.value}")
            else:
                logging.debug(f"Job {job_id} not found in registry")
            return job

    def wait_for_completion(self, job_ids: list[str], timeout: Optional[float] = None) -> bool:
        """Wait for all specified jobs to complete.

        Args:
            job_ids: List of job IDs to wait for
            timeout: Maximum time to wait in seconds (None = infinite)

        Returns:
            True if all jobs completed successfully, False otherwise
        """
        logging.debug(f"Waiting for {len(job_ids)} jobs to complete (timeout: {timeout}s)")
        start_time = time.time()

        while True:
            with self.jobs_lock:
                all_done = all(self.jobs[jid].state in (JobState.COMPLETED, JobState.FAILED, JobState.CANCELLED) for jid in job_ids if jid in self.jobs)
                if all_done:
                    success = all(self.jobs[jid].state == JobState.COMPLETED for jid in job_ids if jid in self.jobs)
                    completed_count = sum(1 for jid in job_ids if jid in self.jobs and self.jobs[jid].state == JobState.COMPLETED)
                    failed_count = sum(1 for jid in job_ids if jid in self.jobs and self.jobs[jid].state == JobState.FAILED)
                    logging.info(f"All jobs completed: {completed_count} succeeded, {failed_count} failed")
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
        logging.debug(f"Attempting to cancel {len(job_ids)} jobs")
        with self.jobs_lock:
            cancelled_count = 0
            skipped_count = 0
            for jid in job_ids:
                if jid in self.jobs:
                    if self.jobs[jid].state == JobState.PENDING:
                        self.jobs[jid].state = JobState.CANCELLED
                        cancelled_count += 1
                        logging.debug(f"Cancelled pending job: {jid}")
                    else:
                        skipped_count += 1
                        logging.debug(f"Cannot cancel job {jid} (state: {self.jobs[jid].state.value})")
                else:
                    logging.debug(f"Job {jid} not found in registry")

            if cancelled_count > 0:
                logging.info(f"Cancelled {cancelled_count} pending jobs ({skipped_count} skipped)")
            else:
                logging.debug(f"No jobs cancelled ({skipped_count} not in pending state)")

    def get_statistics(self) -> dict[str, int]:
        """Get queue statistics.

        Returns:
            Dictionary with job counts by state
        """
        logging.debug("Calculating queue statistics")
        with self.jobs_lock:
            stats = {
                "total_jobs": len(self.jobs),
                "pending": sum(1 for j in self.jobs.values() if j.state == JobState.PENDING),
                "running": sum(1 for j in self.jobs.values() if j.state == JobState.RUNNING),
                "completed": sum(1 for j in self.jobs.values() if j.state == JobState.COMPLETED),
                "failed": sum(1 for j in self.jobs.values() if j.state == JobState.FAILED),
                "cancelled": sum(1 for j in self.jobs.values() if j.state == JobState.CANCELLED),
            }
            success_count = stats["completed"]
            failed_count = stats["failed"]
            success_rate = (success_count / (success_count + failed_count) * 100) if (success_count + failed_count) > 0 else 0.0

        logging.info(f"Queue stats: pending={stats['pending']}, active={stats['running']}, completed={stats['completed']}, failed={stats['failed']}")
        logging.debug(f"Success rate: {success_rate:.1f}% ({success_count} successful, {failed_count} failed)")
        logging.debug(f"Worker utilization: {stats['running']}/{self.num_workers} workers busy")

        return stats

    def get_failed_jobs(self) -> list[CompilationJob]:
        """Get all failed jobs.

        Returns:
            List of failed compilation jobs
        """
        logging.debug("Retrieving failed jobs")
        with self.jobs_lock:
            failed = [j for j in self.jobs.values() if j.state == JobState.FAILED]
            logging.debug(f"Found {len(failed)} failed jobs")
            if failed:
                for job in failed:
                    logging.debug(f"Failed job: {job.job_id} - {job.source_path.name} (exit code: {job.result_code})")
            return failed

    def clear_jobs(self) -> None:
        """Clear all completed/failed/cancelled jobs from registry."""
        logging.debug("Clearing completed/failed/cancelled jobs from registry")
        with self.jobs_lock:
            before_count = len(self.jobs)
            to_remove = [jid for jid, job in self.jobs.items() if job.state in (JobState.COMPLETED, JobState.FAILED, JobState.CANCELLED)]
            logging.debug(f"Registry size before cleanup: {before_count} jobs")
            logging.debug(f"Jobs to remove: {len(to_remove)}")

            for jid in to_remove:
                logging.debug(f"Removing job from registry: {jid}")
                del self.jobs[jid]

            after_count = len(self.jobs)
            logging.debug(f"Registry size after cleanup: {after_count} jobs")

            if to_remove:
                logging.info(f"Cleared {len(to_remove)} completed jobs from registry")
            else:
                logging.debug("No jobs to clear")

    def shutdown(self) -> None:
        """Shutdown worker pool."""
        logging.info("Shutting down CompilationJobQueue")
        logging.debug(f"Remaining jobs in queue: {self.job_queue.qsize()}")
        with self.jobs_lock:
            active_jobs = sum(1 for j in self.jobs.values() if j.state == JobState.RUNNING)
            pending_jobs = sum(1 for j in self.jobs.values() if j.state == JobState.PENDING)
            logging.debug(f"Active jobs: {active_jobs}, Pending jobs: {pending_jobs}")

        logging.debug("Setting running flag to False")
        self.running = False
        logging.debug(f"Waiting for {len(self.workers)} workers to finish")

        for worker in self.workers:
            logging.debug(f"Waiting for worker {worker.name} to join (timeout: 2.0s)")
            worker.join(timeout=2.0)
            if worker.is_alive():
                logging.warning(f"Worker {worker.name} did not finish within timeout")
            else:
                logging.debug(f"Worker {worker.name} joined successfully")

        self.workers.clear()
        logging.debug("Worker list cleared")
        logging.info("CompilationJobQueue shut down")
