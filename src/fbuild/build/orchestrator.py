"""Abstract base class for build orchestrators.

This module defines the interface for platform-specific build orchestrators
to ensure consistent behavior across different platforms.
"""

import contextlib
import logging
import multiprocessing
from abc import ABC, abstractmethod
from pathlib import Path
from typing import TYPE_CHECKING, Optional, Protocol, runtime_checkable
from dataclasses import dataclass

if TYPE_CHECKING:
    from fbuild.build.linker import SizeInfo
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


@runtime_checkable
class PlatformBuildMethod(Protocol):
    """Protocol defining the expected signature for internal _build_XXX() methods.

    Platform orchestrators implement internal build methods (e.g., _build_avr, _build_esp32)
    that follow this protocol signature. This ensures consistent parameter passing across
    all platform-specific implementations.

    The jobs parameter controls parallel compilation:
    - jobs=None: Use all CPU cores (default)
    - jobs=1: Force serial compilation
    - jobs=N: Use N parallel workers

    Example:
        class AVROrchestrator(PlatformOrchestrator):
            def _build_avr(
                self,
                project_path: Path,
                env_name: str,
                target: str,
                verbose: bool,
                clean: bool,
                jobs: int | None = None,
            ) -> BuildResult:
                # Implementation here
                ...
    """

    def __call__(
        self,
        project_path: Path,
        env_name: str,
        target: str,
        verbose: bool,
        clean: bool,
        jobs: int | None = None,
    ) -> BuildResult:
        """Execute platform-specific build.

        Args:
            project_path: Project root directory containing platformio.ini
            env_name: Environment name to build
            target: Build target (e.g., 'flash', 'firmware')
            verbose: Enable verbose output
            clean: Clean build (remove all artifacts before building)
            jobs: Number of parallel compilation jobs (None = CPU count, 1 = serial)

        Returns:
            BuildResult with build status and output paths

        Raises:
            BuildOrchestratorError: If build fails at any phase
        """
        ...


class IBuildOrchestrator(ABC):
    """Interface for build orchestrators.

    Build orchestrators coordinate the entire build process:
    1. Parse configuration
    2. Ensure toolchain/framework packages
    3. Scan source files
    4. Compile sources
    5. Link firmware
    6. Generate binaries

    Implementation Guidelines:
    - Platform-specific implementations should define internal build methods (e.g., _build_avr, _build_esp32)
      that follow the PlatformBuildMethod protocol signature
    - Use the managed_compilation_queue() context manager for automatic resource cleanup when handling
      compilation queues, especially for temporary per-build queues
    - The jobs parameter controls parallel compilation: None=CPU count, 1=serial, N=custom worker count

    Example:
        class AVROrchestrator(IBuildOrchestrator):
            def build(self, project_dir: Path, env_name: Optional[str] = None,
                      clean: bool = False, verbose: Optional[bool] = None,
                      jobs: int | None = None) -> BuildResult:
                with managed_compilation_queue(jobs, verbose=verbose or False) as queue:
                    return self._build_avr(project_dir, env_name, 'firmware',
                                          verbose or False, clean, jobs)
    """

    @abstractmethod
    def build(
        self,
        project_dir: Path,
        env_name: Optional[str] = None,
        clean: bool = False,
        verbose: Optional[bool] = None,
        jobs: int | None = None,
    ) -> BuildResult:
        """Execute complete build process.

        Args:
            project_dir: Project root directory containing platformio.ini
            env_name: Environment name to build (defaults to first/default env)
            clean: Clean build (remove all artifacts before building)
            verbose: Override verbose setting
            jobs: Number of parallel compilation jobs (None = CPU count, 1 = serial)

        Returns:
            BuildResult with build status and output paths

        Raises:
            BuildOrchestratorError: If build fails at any phase
        """
        pass


@contextlib.contextmanager
def managed_compilation_queue(jobs: int | None, verbose: bool = False):
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

    Yields:
        Optional[CompilationJobQueue]: The compilation queue to use, or None for synchronous compilation

    Example:
        with managed_compilation_queue(jobs=4, verbose=True) as queue:
            # Use queue for compilation
            compiler.compile(..., compilation_queue=queue)
        # Queue is automatically cleaned up here if it was temporary

    Notes:
        - Serial mode (jobs=1): Yields None, no cleanup needed
        - Default parallelism (jobs=None): Yields daemon's shared queue, no cleanup needed
        - Custom worker count: Creates temporary queue, automatically cleaned up on exit
        - Exceptions during shutdown are logged but don't mask the original exception
    """
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
    """Get appropriate compilation queue based on jobs parameter.

    This function implements the strategy for parallel compilation:
    1. jobs=1 (serial): Return None (synchronous compilation)
    2. jobs=None or jobs=cpu_count: Use daemon's shared queue if available
    3. jobs=N (custom): Create temporary per-build queue with N workers

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
    # Case 1: Serial compilation (jobs=1)
    if jobs == 1:
        if verbose:
            print("[Sync Mode] Using serial compilation (jobs=1)")
        return None, False

    cpu_count = multiprocessing.cpu_count()

    # Case 2: Default parallelism (jobs=None or jobs=cpu_count) - use daemon's shared queue
    if jobs is None or jobs == cpu_count:
        try:
            from fbuild.daemon.daemon import get_compilation_queue
            daemon_queue = get_compilation_queue()
            if daemon_queue:
                if verbose:
                    print(f"[Async Mode] Using daemon compilation queue with {daemon_queue.num_workers} workers")
                return daemon_queue, False
            else:
                if verbose:
                    print("[Sync Mode] Daemon queue not available, using synchronous compilation")
                return None, False
        except (ImportError, AttributeError):
            if verbose:
                print("[Sync Mode] Daemon not available, using synchronous compilation")
            return None, False

    # Case 3: Custom worker count (1 < jobs < cpu_count or jobs > cpu_count)
    # Create a temporary per-build queue with the requested worker count
    try:
        from fbuild.daemon.compilation_queue import CompilationJobQueue

        if verbose:
            print(f"[Async Mode] Creating temporary compilation queue with {jobs} workers")

        temp_queue = CompilationJobQueue(num_workers=jobs)
        temp_queue.start()
        return temp_queue, True  # True = caller must cleanup
    except (ImportError, AttributeError) as e:
        logging.warning(f"Failed to create temporary compilation queue: {e}")
        if verbose:
            print("[Sync Mode] Falling back to synchronous compilation")
        return None, False
