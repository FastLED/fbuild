"""Abstract base class for build orchestrators.

This module defines the interface for platform-specific build orchestrators
to ensure consistent behavior across different platforms.
"""

import contextlib
import logging
import multiprocessing
from abc import ABC, abstractmethod
from pathlib import Path
from typing import TYPE_CHECKING, Optional
from dataclasses import dataclass

if TYPE_CHECKING:
    from fbuild.build.linker import SizeInfo
    from fbuild.build.build_context import BuildParams
    from fbuild.daemon.compilation_queue import CompilationJobQueue


@dataclass
class BuildResult:
    """Result of a complete build operation."""

    success: bool
    hex_path: Optional[Path]  # For AVR: .hex, For ESP32: .bin
    elf_path: Optional[Path]
    size_info: Optional["SizeInfo"]
    build_time: float
    message: str


class BuildOrchestratorError(Exception):
    """Base exception for build orchestration errors."""
    pass




class IBuildOrchestrator(ABC):
    """Interface for build orchestrators.

    Build orchestrators coordinate the entire build process:
    1. Parse configuration
    2. Ensure toolchain/framework packages
    3. Scan source files
    4. Compile sources
    5. Link firmware
    6. Generate binaries

    The build method receives a BuildRequest with basic parameters:
    - project_dir, env_name, clean
    - profile and pre-resolved profile_flags
    - compilation queue (mandatory)
    - build_dir (incorporating profile name)
    - verbose setting

    The orchestrator creates a full BuildContext after platform initialization,
    adding platform, toolchain, mcu, framework_version, and compilation_executor.
    """

    @abstractmethod
    def build(self, request: "BuildParams") -> BuildResult:
        """Execute complete build process.

        Args:
            request: Build request with basic parameters from build_processor

        Returns:
            BuildResult with build status and output paths

        Raises:
            BuildOrchestratorError: If build fails at any phase
        """
        pass


@contextlib.contextmanager
def managed_compilation_queue(jobs: int | None, verbose: bool = False, provided_queue: Optional["CompilationJobQueue"] = None):
    """Context manager for safely managing compilation queue lifecycle.

    This context manager obtains a compilation queue using get_compilation_queue_for_build()
    and ensures proper cleanup of temporary queues. It handles resource management automatically,
    preventing resource leaks when temporary per-build queues are created.

    The context manager yields the compilation queue (or None for synchronous mode) and ensures
    that temporary queues are properly shut down when the context exits, even if an exception
    occurs during the build process.

    Args:
        jobs: Number of parallel compilation jobs (None = CPU count, 1 = serial, N = custom)
        verbose: Whether to log queue selection and lifecycle events
        provided_queue: Queue from daemon context (bypasses queue creation logic)

    Yields:
        Optional[CompilationJobQueue]: The compilation queue to use, or None for synchronous compilation

    Example:
        with managed_compilation_queue(jobs=4, verbose=True) as queue:
            # Use queue for compilation
            compiler.compile(..., compilation_queue=queue)
        # Queue is automatically cleaned up here if it was temporary

    Notes:
        - Provided queue (from daemon): Used directly, no cleanup needed
        - Serial mode (jobs=1): Yields None, no cleanup needed
        - Default parallelism (jobs=None): Yields daemon's shared queue, no cleanup needed
        - Custom worker count: Creates temporary queue, automatically cleaned up on exit
        - Exceptions during shutdown are logged but don't mask the original exception
    """
    # If queue provided explicitly (from build_processor), use it
    if provided_queue is not None:
        if verbose:
            print(f"[Parallel Mode] Using provided queue with {provided_queue.num_workers} workers")
        yield provided_queue  # No cleanup (managed by daemon)
        return

    # Otherwise, follow existing logic (for testing/standalone)
    queue, should_cleanup = get_compilation_queue_for_build(jobs, verbose)
    try:
        yield queue
    finally:
        if should_cleanup and queue:
            try:
                if verbose:
                    print(f"[Cleanup] Shutting down temporary compilation queue with {queue.num_workers} workers")
                queue.shutdown()
            except KeyboardInterrupt:  # noqa: KBI002
                # Re-raise keyboard interrupts (KBI002 suppressed: bare raise is safe)
                raise
            except Exception as e:
                # Log the error but don't mask the original exception
                logging.error(f"Error during compilation queue cleanup: {e}")


def get_compilation_queue_for_build(jobs: int | None, verbose: bool = False) -> tuple[Optional["CompilationJobQueue"], bool]:
    """Get appropriate compilation queue for standalone builds.

    NOTE: In production, build_processor passes queue via
    managed_compilation_queue(provided_queue=...). This function
    is only for testing/standalone builds.

    This function implements the strategy for parallel compilation:
    1. jobs=1 (serial): Return None (synchronous compilation)
    2. jobs=N (custom or None): Create temporary per-build queue with N workers

    Args:
        jobs: Number of parallel compilation jobs (None = CPU count, 1 = serial, N = custom)
        verbose: Whether to log queue selection

    Returns:
        Tuple of (compilation_queue, should_cleanup):
        - compilation_queue: Queue to use for compilation, or None for synchronous
        - should_cleanup: True if this is a temporary queue that must be cleaned up after build

    Example:
        >>> queue, cleanup = get_compilation_queue_for_build(jobs=4, verbose=True)
        >>> # ... use queue for compilation ...
        >>> if cleanup and queue:
        >>>     queue.shutdown()
    """
    # Case 1: Serial compilation (jobs=1) - explicitly requested
    if jobs == 1:
        if verbose:
            print("[Serial Mode] Using synchronous compilation (jobs=1)")
        return None, False

    # Case 2: Create temporary queue (daemon queue passed explicitly)
    from fbuild.daemon.compilation_queue import CompilationJobQueue

    cpu_count = multiprocessing.cpu_count()
    worker_count = jobs if jobs is not None else cpu_count

    if verbose:
        print(f"[Parallel Mode] Creating temporary queue with {worker_count} workers")

    temp_queue = CompilationJobQueue(num_workers=worker_count)
    temp_queue.start()
    return temp_queue, True  # True = caller must cleanup
