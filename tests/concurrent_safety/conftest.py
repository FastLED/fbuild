"""
Shared fixtures for concurrent safety tests.

This module provides pytest fixtures for testing concurrent safety of
fbuild daemon operations including lock management, port state tracking,
and process spawning.

These tests are skipped by default - run with `pytest --full` to include them.
"""

import subprocess
import sys
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Generator

import pytest


def pytest_configure(config: Any) -> None:
    """Configure pytest markers for concurrent tests."""
    config.addinivalue_line("markers", "concurrent: concurrent safety tests")
    config.addinivalue_line("markers", "hardware: requires ESP32 hardware")
    config.addinivalue_line("markers", "single_device: requires 1 ESP32-C6 device")


def pytest_collection_modifyitems(items: list[Any]) -> None:
    """Mark all tests in concurrent_safety directory with concurrent_safety marker.

    This ensures these tests are skipped by default and only run with --full.
    """
    for item in items:
        # Check if test is from concurrent_safety directory
        if "concurrent_safety" in str(item.fspath):
            item.add_marker(pytest.mark.concurrent_safety)


@pytest.fixture
def lock_manager() -> Any:
    """Fresh ResourceLockManager for each test."""
    from fbuild.daemon.lock_manager import ResourceLockManager

    return ResourceLockManager()


@pytest.fixture
def port_state_manager() -> Any:
    """Fresh PortStateManager for each test."""
    from fbuild.daemon.port_state_manager import PortStateManager

    return PortStateManager()


@pytest.fixture
def project_root() -> Path:
    """Path to the fbuild project root."""
    return Path(__file__).parent.parent.parent


@pytest.fixture
def esp32c6_project(project_root: Path) -> Path:
    """Path to ESP32-C6 test project."""
    return project_root / "tests" / "esp32c6"


@pytest.fixture
def esp32dev_project(project_root: Path) -> Path:
    """Path to ESP32 Dev test project."""
    return project_root / "tests" / "esp32dev"


@pytest.fixture
def uno_project(project_root: Path) -> Path:
    """Path to Arduino Uno test project."""
    return project_root / "tests" / "uno"


@dataclass
class ProcessResult:
    """Result of a subprocess execution."""

    returncode: int
    stdout: str
    stderr: str
    elapsed_time: float


class FbuildProcessSpawner:
    """Helper for spawning fbuild processes in tests.

    This class provides methods to spawn fbuild commands as subprocesses
    and wait for specific output patterns.
    """

    def __init__(self) -> None:
        """Initialize the process spawner."""
        self._processes: list[subprocess.Popen[str]] = []

    def spawn_build(
        self,
        project: Path,
        env: str,
        clean: bool = False,
        verbose: bool = False,
    ) -> subprocess.Popen[str]:
        """Spawn a build process.

        Args:
            project: Path to the project directory
            env: Environment name
            clean: Whether to perform a clean build
            verbose: Whether to enable verbose output

        Returns:
            The spawned Popen object
        """
        cmd = [sys.executable, "-m", "fbuild", "build", str(project), "-e", env]
        if clean:
            cmd.append("-c")
        if verbose:
            cmd.append("-v")

        proc = subprocess.Popen(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        self._processes.append(proc)
        return proc

    def spawn_deploy(
        self,
        project: Path,
        env: str,
        port: str | None = None,
        clean: bool = False,
        monitor: bool = False,
        monitor_timeout: float | None = None,
    ) -> subprocess.Popen[str]:
        """Spawn a deploy process.

        Args:
            project: Path to the project directory
            env: Environment name
            port: Serial port to use (None for auto-detect)
            clean: Whether to perform a clean build
            monitor: Whether to start monitor after deploy
            monitor_timeout: Timeout for monitor in seconds

        Returns:
            The spawned Popen object
        """
        cmd = [sys.executable, "-m", "fbuild", "deploy", str(project), "-e", env]
        if port:
            cmd.extend(["-p", port])
        if clean:
            cmd.append("-c")
        if monitor:
            cmd.append("--monitor")
            if monitor_timeout:
                cmd.append(f"--timeout={monitor_timeout}")

        proc = subprocess.Popen(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        self._processes.append(proc)
        return proc

    def spawn_monitor(
        self,
        project: Path,
        env: str,
        port: str,
        baud: int | None = None,
        timeout: float | None = None,
    ) -> subprocess.Popen[str]:
        """Spawn a monitor process.

        Args:
            project: Path to the project directory
            env: Environment name
            port: Serial port to monitor
            baud: Baud rate (None for default)
            timeout: Timeout in seconds (None for no timeout)

        Returns:
            The spawned Popen object
        """
        cmd = [
            sys.executable,
            "-m",
            "fbuild",
            "monitor",
            str(project),
            "-e",
            env,
            "-p",
            port,
        ]
        if baud:
            cmd.extend(["-b", str(baud)])
        if timeout:
            cmd.extend(["-t", str(timeout)])

        proc = subprocess.Popen(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        self._processes.append(proc)
        return proc

    def wait_for_output(
        self,
        proc: subprocess.Popen[str],
        pattern: str,
        timeout: float = 30.0,
    ) -> bool:
        """Wait for a pattern in process output.

        Args:
            proc: The process to monitor
            pattern: String pattern to look for
            timeout: Maximum time to wait in seconds

        Returns:
            True if pattern was found, False if timeout
        """
        start_time = time.time()
        output_buffer = ""

        while time.time() - start_time < timeout:
            if proc.stdout is None:
                return False

            # Read available output
            try:
                line = proc.stdout.readline()
                if line:
                    output_buffer += line
                    if pattern in output_buffer:
                        return True
            except Exception:
                pass

            # Check if process has ended
            if proc.poll() is not None:
                # Read remaining output
                remaining = proc.stdout.read()
                if remaining:
                    output_buffer += remaining
                return pattern in output_buffer

            time.sleep(0.1)

        return False

    def wait_and_get_output(
        self,
        proc: subprocess.Popen[str],
        timeout: float = 60.0,
    ) -> ProcessResult:
        """Wait for a process to complete and get its output.

        Args:
            proc: The process to wait for
            timeout: Maximum time to wait in seconds

        Returns:
            ProcessResult with return code, stdout, stderr, and elapsed time
        """
        start_time = time.time()
        try:
            stdout, stderr = proc.communicate(timeout=timeout)
            elapsed = time.time() - start_time
            return ProcessResult(
                returncode=proc.returncode,
                stdout=stdout or "",
                stderr=stderr or "",
                elapsed_time=elapsed,
            )
        except subprocess.TimeoutExpired:
            proc.kill()
            stdout, stderr = proc.communicate()
            elapsed = time.time() - start_time
            return ProcessResult(
                returncode=-1,
                stdout=stdout or "",
                stderr=stderr or "",
                elapsed_time=elapsed,
            )

    def cleanup(self) -> None:
        """Kill all spawned processes."""
        for proc in self._processes:
            if proc.poll() is None:
                proc.kill()
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    pass
        self._processes.clear()


@pytest.fixture
def spawner() -> Generator[FbuildProcessSpawner, None, None]:
    """Helper for spawning fbuild processes."""
    spawn = FbuildProcessSpawner()
    yield spawn
    spawn.cleanup()


class ThreadRunner:
    """Helper for running functions in threads and collecting results.

    This class makes it easy to run multiple operations concurrently
    and collect their results and any exceptions.
    """

    def __init__(self) -> None:
        """Initialize the thread runner."""
        self._results: dict[str, Any] = {}
        self._errors: dict[str, Exception] = {}
        self._threads: list[threading.Thread] = []

    def run_in_thread(
        self,
        name: str,
        func: Callable[[], Any],
    ) -> threading.Thread:
        """Run a function in a named thread.

        Args:
            name: Name for the thread/result
            func: Function to run

        Returns:
            The created Thread object
        """

        def wrapper() -> None:
            try:
                self._results[name] = func()
            except Exception as e:
                self._errors[name] = e

        thread = threading.Thread(target=wrapper, name=name)
        self._threads.append(thread)
        return thread

    def start_all(self) -> None:
        """Start all registered threads."""
        for thread in self._threads:
            thread.start()

    def join_all(self, timeout: float | None = None) -> None:
        """Wait for all threads to complete.

        Args:
            timeout: Maximum time to wait per thread
        """
        for thread in self._threads:
            thread.join(timeout=timeout)

    def get_result(self, name: str) -> Any:
        """Get the result from a named thread.

        Args:
            name: Name of the thread

        Returns:
            The result from the thread, or raises the exception if one occurred
        """
        if name in self._errors:
            raise self._errors[name]
        return self._results.get(name)

    def has_error(self, name: str) -> bool:
        """Check if a thread had an error.

        Args:
            name: Name of the thread

        Returns:
            True if the thread raised an exception
        """
        return name in self._errors

    def get_error(self, name: str) -> Exception | None:
        """Get the exception from a named thread.

        Args:
            name: Name of the thread

        Returns:
            The exception if one occurred, None otherwise
        """
        return self._errors.get(name)

    @property
    def all_results(self) -> dict[str, Any]:
        """Get all results."""
        return dict(self._results)

    @property
    def all_errors(self) -> dict[str, Exception]:
        """Get all errors."""
        return dict(self._errors)


@pytest.fixture
def thread_runner() -> ThreadRunner:
    """Helper for running concurrent operations."""
    return ThreadRunner()


@pytest.fixture
def mock_daemon_context(lock_manager: Any, port_state_manager: Any) -> Any:
    """Create a mock daemon context for testing.

    This fixture creates a minimal daemon context with real lock_manager
    and port_state_manager instances for testing lock behavior.
    """
    from dataclasses import dataclass, field
    from threading import Lock
    from unittest.mock import MagicMock

    @dataclass
    class MockDaemonContext:
        """Minimal daemon context for testing."""

        daemon_pid: int = 12345
        daemon_started_at: float = field(default_factory=time.time)
        lock_manager: Any = None
        port_state_manager: Any = None
        operation_in_progress: bool = False
        operation_lock: Lock = field(default_factory=Lock)
        status_manager: MagicMock = field(default_factory=MagicMock)
        compilation_queue: MagicMock = field(default_factory=MagicMock)
        operation_registry: MagicMock = field(default_factory=MagicMock)
        subprocess_manager: MagicMock = field(default_factory=MagicMock)
        file_cache: MagicMock = field(default_factory=MagicMock)
        error_collector: MagicMock = field(default_factory=MagicMock)

    return MockDaemonContext(
        lock_manager=lock_manager,
        port_state_manager=port_state_manager,
    )
