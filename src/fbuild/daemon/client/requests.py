"""
Request Handling for Daemon Operations

Handles build, deploy, monitor, and install dependencies requests.
Implements request submission, progress monitoring, and output tailing.
"""

import json
import os
import time
from abc import ABC, abstractmethod
from pathlib import Path
from typing import Any

from fbuild.daemon.messages import (
    BuildRequest,
    DaemonState,
    DaemonStatus,
    DeployRequest,
    InstallDependenciesRequest,
    MonitorRequest,
)

from .lifecycle import (
    BUILD_REQUEST_FILE,
    DAEMON_DIR,
    DEPLOY_REQUEST_FILE,
    INSTALL_DEPS_REQUEST_FILE,
    MONITOR_REQUEST_FILE,
    ensure_daemon_running,
    is_daemon_running,
)
from .status import display_spinner_progress, display_status, read_status_file


def write_request_file(request_file: Path, request: Any) -> None:
    """Atomically write request file.

    Args:
        request_file: Path to request file
        request: Request object (DeployRequest or MonitorRequest)
    """
    DAEMON_DIR.mkdir(parents=True, exist_ok=True)

    # Atomic write using temporary file
    temp_file = request_file.with_suffix(".tmp")
    with open(temp_file, "w") as f:
        json.dump(request.to_dict(), f, indent=2)

    # Atomic rename
    temp_file.replace(request_file)


def _display_monitor_summary(project_dir: Path) -> None:
    """Display monitor summary from JSON file.

    Args:
        project_dir: Project directory where summary file is located
    """
    summary_file = project_dir / ".fbuild" / "monitor_summary.json"
    if not summary_file.exists():
        return

    try:
        with open(summary_file, "r", encoding="utf-8") as f:
            summary = json.load(f)

        print("\n" + "=" * 50)
        print("Monitor Summary")
        print("=" * 50)

        # Display expect pattern result
        if summary.get("expect_pattern"):
            pattern = summary["expect_pattern"]
            found = summary.get("expect_found", False)
            status = "FOUND ✓" if found else "NOT FOUND ✗"
            print(f'Expected pattern: "{pattern}" - {status}')

        # Display halt on error pattern result
        if summary.get("halt_on_error_pattern"):
            pattern = summary["halt_on_error_pattern"]
            found = summary.get("halt_on_error_found", False)
            status = "FOUND ✗" if found else "NOT FOUND ✓"
            print(f'Error pattern: "{pattern}" - {status}')

        # Display halt on success pattern result
        if summary.get("halt_on_success_pattern"):
            pattern = summary["halt_on_success_pattern"]
            found = summary.get("halt_on_success_found", False)
            status = "FOUND ✓" if found else "NOT FOUND ✗"
            print(f'Success pattern: "{pattern}" - {status}')

        # Display statistics
        lines = summary.get("lines_processed", 0)
        elapsed = summary.get("elapsed_time", 0.0)
        exit_reason = summary.get("exit_reason", "unknown")

        print(f"Lines processed: {lines}")
        print(f"Time elapsed: {elapsed:.2f}s")

        # Translate exit_reason to user-friendly text
        reason_text = {
            "timeout": "Timeout reached",
            "expect_found": "Expected pattern found",
            "halt_error": "Error pattern detected",
            "halt_success": "Success pattern detected",
            "interrupted": "Interrupted by user",
            "error": "Serial port error",
        }.get(exit_reason, exit_reason)

        print(f"Exit reason: {reason_text}")
        print("=" * 50)

    except KeyboardInterrupt:
        raise
    except Exception:
        # Silently fail - don't disrupt the user experience
        pass


# ============================================================================
# REQUEST HANDLER ARCHITECTURE
# ============================================================================


class BaseRequestHandler(ABC):
    """Base class for handling daemon requests with common functionality.

    Implements the template method pattern to eliminate duplication across
    build, deploy, and monitor request handlers.
    """

    def __init__(self, project_dir: Path, environment: str, timeout: float = 1800):
        """Initialize request handler.

        Args:
            project_dir: Project directory
            environment: Build environment
            timeout: Maximum wait time in seconds (default: 30 minutes)
        """
        self.project_dir = project_dir
        self.environment = environment
        self.timeout = timeout
        self.start_time = 0.0
        self.last_message: str | None = None
        self.monitoring_started = False
        self.build_output_started = False
        self.output_file_position = 0
        self.spinner_idx = 0
        self.last_spinner_update = 0.0
        self.seen_completion = False  # Track if we've seen a COMPLETED status

    @abstractmethod
    def create_request(self) -> BuildRequest | DeployRequest | InstallDependenciesRequest | MonitorRequest:
        """Create the specific request object.

        Returns:
            Request object (BuildRequest, DeployRequest, InstallDependenciesRequest, or MonitorRequest)
        """
        pass

    @abstractmethod
    def get_request_file(self) -> Path:
        """Get the request file path.

        Returns:
            Path to request file
        """
        pass

    @abstractmethod
    def get_operation_name(self) -> str:
        """Get the operation name for display.

        Returns:
            Operation name (e.g., "Build", "Deploy", "Monitor")
        """
        pass

    @abstractmethod
    def get_operation_emoji(self) -> str:
        """Get the operation emoji for display.

        Returns:
            Operation emoji (e.g., "🔨", "📦", "👁️")
        """
        pass

    def should_tail_output(self) -> bool:
        """Check if output file should be tailed.

        Returns:
            True if output should be tailed, False otherwise
        """
        return False

    def get_output_file_path(self) -> Path:
        """Get the output file path to tail. Override in subclasses.

        Returns:
            Path to the output file
        """
        return self.project_dir / ".fbuild" / "monitor_output.txt"

    def on_monitoring_started(self) -> None:
        """Hook called when monitoring phase starts."""
        pass

    def on_completion(self, elapsed: float) -> None:
        """Hook called on successful completion.

        Args:
            elapsed: Elapsed time in seconds
        """
        pass

    def on_failure(self, status: DaemonStatus, elapsed: float) -> None:
        """Hook called on failure.

        Args:
            status: Current daemon status
            elapsed: Elapsed time in seconds
        """
        pass

    def print_submission_info(self) -> None:
        """Print request submission information."""
        print(f"\n📤 Submitting {self.get_operation_name().lower()} request...")
        print(f"   Project: {self.project_dir}")
        print(f"   Environment: {self.environment}")

    def tail_output_file(self) -> None:
        """Tail the output file and print new lines."""
        output_file = self.get_output_file_path()
        if output_file.exists():
            try:
                with open(output_file, "r", encoding="utf-8", errors="replace") as f:
                    f.seek(self.output_file_position)
                    new_lines = f.read()
                    if new_lines:
                        print(new_lines, end="", flush=True)
                        self.output_file_position = f.tell()
            except KeyboardInterrupt:
                raise
            except Exception:
                pass  # Ignore read errors

    def read_remaining_output(self) -> None:
        """Read any remaining output from output file."""
        if not self.monitoring_started and not self.build_output_started:
            return

        output_file = self.get_output_file_path()
        if output_file.exists():
            try:
                with open(output_file, "r", encoding="utf-8", errors="replace") as f:
                    f.seek(self.output_file_position)
                    new_lines = f.read()
                    if new_lines:
                        print(new_lines, end="", flush=True)
            except KeyboardInterrupt:
                raise
            except Exception:
                pass

    def handle_keyboard_interrupt(self, request_id: str) -> bool:
        """Handle keyboard interrupt with background option.

        Args:
            request_id: Request ID for cancellation

        Returns:
            False (operation not completed or cancelled)
        """
        print("\n\n⚠️  Interrupted by user (Ctrl-C)")
        response = input("Keep operation running in background? (y/n): ").strip().lower()

        if response in ("y", "yes"):
            print("\n✅ Operation continues in background")
            print("   Check status: fbuild daemon status")
            print("   Stop daemon: fbuild daemon stop")
            return False
        else:
            print("\n🛑 Requesting daemon to stop operation...")
            cancel_file = DAEMON_DIR / f"cancel_{request_id}.signal"
            cancel_file.touch()
            print("   ✅ Cancellation signal sent")
            print("   Daemon will detect and abort operation within ~1 second")
            return False

    def execute(self) -> bool:
        """Execute the request and monitor progress.

        Returns:
            True if operation successful, False otherwise
        """
        # Ensure daemon is running
        try:
            ensure_daemon_running()
        except RuntimeError as e:
            print(f"❌ {e}")
            return False

        # Print submission info
        self.print_submission_info()

        # Create and submit request
        request = self.create_request()
        write_request_file(self.get_request_file(), request)
        print(f"   Request ID: {request.request_id}")
        print("   ✅ Submitted\n")

        # Monitor progress
        print(f"{self.get_operation_emoji()} {self.get_operation_name()} Progress:")
        self.start_time = time.time()

        while True:
            try:
                elapsed = time.time() - self.start_time

                # Check timeout
                if elapsed > self.timeout:
                    print(f"\n❌ {self.get_operation_name()} timeout ({self.timeout}s)")
                    return False

                # Read status
                status = read_status_file()

                # Display progress when message changes
                if status.message != self.last_message:
                    # Clear spinner line before new status message
                    if self.last_message is not None:
                        print("\r" + " " * 80 + "\r", end="", flush=True)
                    display_status(status)
                    self.last_message = status.message
                    self.last_spinner_update = time.time()
                else:
                    # Show spinner with elapsed time when in building/deploying state
                    # Only show spinner if we're not tailing build output
                    if status.state in (DaemonState.BUILDING, DaemonState.DEPLOYING):
                        if not (self.should_tail_output() and status.state == DaemonState.BUILDING and self.build_output_started):
                            current_time = time.time()
                            # Update spinner every 100ms
                            if current_time - self.last_spinner_update >= 0.1:
                                self.spinner_idx += 1
                                display_spinner_progress(status, elapsed, self.spinner_idx)
                                self.last_spinner_update = current_time

                # Handle build output tailing phase
                if self.should_tail_output() and status.state == DaemonState.BUILDING:
                    if not self.build_output_started:
                        self.build_output_started = True
                        # Clear spinner line before build output
                        print("\r" + " " * 80 + "\r", end="", flush=True)
                        print()  # Blank line before build output
                    self.tail_output_file()

                # Handle monitoring phase
                if self.should_tail_output() and status.state == DaemonState.MONITORING:
                    if not self.monitoring_started:
                        self.monitoring_started = True
                        # Reset file position when transitioning from build to monitor
                        if self.build_output_started:
                            self.output_file_position = 0
                        # Clear spinner line before monitor output
                        print("\r" + " " * 80 + "\r", end="", flush=True)
                        print()  # Blank line before serial output
                        self.on_monitoring_started()

                if self.monitoring_started and self.should_tail_output():
                    self.tail_output_file()

                # Check completion
                if status.state == DaemonState.COMPLETED:
                    if status.request_id == request.request_id:
                        self.seen_completion = True
                        self.read_remaining_output()
                        self.on_completion(elapsed)
                        # Clear spinner line before completion message
                        print("\r" + " " * 80 + "\r", end="", flush=True)
                        print(f"✅ {self.get_operation_name()} completed in {elapsed:.1f}s")
                        return True

                elif status.state == DaemonState.FAILED:
                    if status.request_id == request.request_id:
                        self.read_remaining_output()
                        self.on_failure(status, elapsed)
                        # Clear spinner line before failure message
                        print("\r" + " " * 80 + "\r", end="", flush=True)
                        print(f"❌ {self.get_operation_name()} failed: {status.message}")
                        return False

                # Check if daemon has stopped running (PID no longer exists)
                # This handles cases where daemon shuts down but status file preserves final state
                if not is_daemon_running():
                    # Daemon stopped - determine if our request was processed
                    # Strategy 1: Check if we previously saw COMPLETED status with our request_id
                    if self.seen_completion:
                        self.read_remaining_output()
                        self.on_completion(elapsed)
                        print("\r" + " " * 80 + "\r", end="", flush=True)
                        print(f"✅ {self.get_operation_name()} completed in {elapsed:.1f}s")
                        return True

                    # Strategy 2: Check if status file shows COMPLETED (even with different/missing request_id)
                    # This handles the race where daemon completes and updates status, but we haven't seen it yet
                    if status.state == DaemonState.COMPLETED:
                        # Status is COMPLETED - likely our request succeeded
                        # (daemon wouldn't be shutting down with COMPLETED status for a different request)
                        self.read_remaining_output()
                        self.on_completion(elapsed)
                        print("\r" + " " * 80 + "\r", end="", flush=True)
                        print(f"✅ {self.get_operation_name()} completed in {elapsed:.1f}s")
                        return True

                    # Strategy 3: Check if request file still exists with our request_id
                    # If it does, our request was definitely NOT processed
                    request_file = self.get_request_file()
                    if request_file.exists():
                        try:
                            with open(request_file) as f:
                                pending_request_data = json.load(f)
                                pending_request_id = pending_request_data.get("request_id")
                                if pending_request_id == request.request_id:
                                    # Our exact request is still pending - not processed
                                    print("\r" + " " * 80 + "\r", end="", flush=True)
                                    print(f"❌ Daemon shut down without processing {self.get_operation_name().lower()} request")
                                    return False
                        except KeyboardInterrupt:
                            raise
                        except Exception:
                            pass  # Can't read request file - continue to assume success

                    # Strategy 4: Request file doesn't exist or has different request_id
                    # Combined with daemon shutdown, this likely means our request was processed
                    # (daemon consumed our request file and processed it)
                    self.read_remaining_output()
                    self.on_completion(elapsed)
                    print("\r" + " " * 80 + "\r", end="", flush=True)
                    print(f"✅ {self.get_operation_name()} completed in {elapsed:.1f}s")
                    return True

                # Sleep before next poll
                poll_interval = 0.1 if self.monitoring_started else 0.1  # Faster polling for spinner
                time.sleep(poll_interval)

            except KeyboardInterrupt:
                # Clear spinner line before interrupt handling
                print("\r" + " " * 80 + "\r", end="", flush=True)
                return self.handle_keyboard_interrupt(request.request_id)


class BuildRequestHandler(BaseRequestHandler):
    """Handler for build requests."""

    def __init__(
        self,
        project_dir: Path,
        environment: str,
        clean_build: bool = False,
        verbose: bool = False,
        timeout: float = 1800,
        jobs: int | None = None,
    ):
        """Initialize build request handler.

        Args:
            project_dir: Project directory
            environment: Build environment
            clean_build: Whether to perform clean build
            verbose: Enable verbose build output
            timeout: Maximum wait time in seconds
            jobs: Number of parallel compilation jobs (default: CPU count, 1 for serial)
        """
        super().__init__(project_dir, environment, timeout)
        self.clean_build = clean_build
        self.verbose = verbose
        self.jobs = jobs

    def create_request(self) -> BuildRequest:
        """Create build request."""
        return BuildRequest(
            project_dir=str(self.project_dir.absolute()),
            environment=self.environment,
            clean_build=self.clean_build,
            verbose=self.verbose,
            caller_pid=os.getpid(),
            caller_cwd=os.getcwd(),
            jobs=self.jobs,
        )

    def get_request_file(self) -> Path:
        """Get build request file path."""
        return BUILD_REQUEST_FILE

    def get_operation_name(self) -> str:
        """Get operation name."""
        return "Build"

    def get_operation_emoji(self) -> str:
        """Get operation emoji."""
        return "🔨"

    def should_tail_output(self) -> bool:
        """Build operations should tail output."""
        return True

    def get_output_file_path(self) -> Path:
        """Build output goes to build_output.txt."""
        return self.project_dir / ".fbuild" / "build_output.txt"

    def print_submission_info(self) -> None:
        """Print build submission information."""
        super().print_submission_info()
        if self.clean_build:
            print("   Clean build: Yes")


class DeployRequestHandler(BaseRequestHandler):
    """Handler for deploy requests."""

    def __init__(
        self,
        project_dir: Path,
        environment: str,
        port: str | None = None,
        clean_build: bool = False,
        monitor_after: bool = False,
        monitor_timeout: float | None = None,
        monitor_halt_on_error: str | None = None,
        monitor_halt_on_success: str | None = None,
        monitor_expect: str | None = None,
        monitor_show_timestamp: bool = False,
        skip_build: bool = False,
        timeout: float = 1800,
    ):
        """Initialize deploy request handler.

        Args:
            project_dir: Project directory
            environment: Build environment
            port: Serial port (optional)
            clean_build: Whether to perform clean build
            monitor_after: Whether to start monitor after deploy
            monitor_timeout: Timeout for monitor
            monitor_halt_on_error: Pattern to halt on error
            monitor_halt_on_success: Pattern to halt on success
            monitor_expect: Expected pattern to check
            monitor_show_timestamp: Whether to prefix output lines with elapsed time
            skip_build: Whether to skip the build phase (upload-only mode)
            timeout: Maximum wait time in seconds
        """
        super().__init__(project_dir, environment, timeout)
        self.port = port
        self.clean_build = clean_build
        self.monitor_after = monitor_after
        self.monitor_timeout = monitor_timeout
        self.monitor_halt_on_error = monitor_halt_on_error
        self.monitor_halt_on_success = monitor_halt_on_success
        self.monitor_expect = monitor_expect
        self.monitor_show_timestamp = monitor_show_timestamp
        self.skip_build = skip_build

    def create_request(self) -> DeployRequest:
        """Create deploy request."""
        return DeployRequest(
            project_dir=str(self.project_dir.absolute()),
            environment=self.environment,
            port=self.port,
            clean_build=self.clean_build,
            monitor_after=self.monitor_after,
            monitor_timeout=self.monitor_timeout,
            monitor_halt_on_error=self.monitor_halt_on_error,
            monitor_halt_on_success=self.monitor_halt_on_success,
            monitor_expect=self.monitor_expect,
            monitor_show_timestamp=self.monitor_show_timestamp,
            skip_build=self.skip_build,
            caller_pid=os.getpid(),
            caller_cwd=os.getcwd(),
        )

    def get_request_file(self) -> Path:
        """Get deploy request file path."""
        return DEPLOY_REQUEST_FILE

    def get_operation_name(self) -> str:
        """Get operation name."""
        return "Deploy"

    def get_operation_emoji(self) -> str:
        """Get operation emoji."""
        return "📦"

    def should_tail_output(self) -> bool:
        """Check if output should be tailed."""
        # Always tail during build phase, and during monitor if monitor_after is set
        return True

    def get_output_file_path(self) -> Path:
        """During build phase, use build_output.txt; during monitor, use monitor_output.txt."""
        status = read_status_file()
        if status.state == DaemonState.BUILDING:
            return self.project_dir / ".fbuild" / "build_output.txt"
        return self.project_dir / ".fbuild" / "monitor_output.txt"

    def print_submission_info(self) -> None:
        """Print deploy submission information."""
        super().print_submission_info()
        if self.port:
            print(f"   Port: {self.port}")

    def on_completion(self, elapsed: float) -> None:
        """Handle completion with monitor summary."""
        if self.monitoring_started:
            _display_monitor_summary(self.project_dir)

    def on_failure(self, status: DaemonStatus, elapsed: float) -> None:
        """Handle failure with monitor summary."""
        if self.monitoring_started:
            _display_monitor_summary(self.project_dir)


class MonitorRequestHandler(BaseRequestHandler):
    """Handler for monitor requests."""

    def __init__(
        self,
        project_dir: Path,
        environment: str,
        port: str | None = None,
        baud_rate: int | None = None,
        halt_on_error: str | None = None,
        halt_on_success: str | None = None,
        expect: str | None = None,
        timeout: float | None = None,
        show_timestamp: bool = False,
    ):
        """Initialize monitor request handler.

        Args:
            project_dir: Project directory
            environment: Build environment
            port: Serial port (optional)
            baud_rate: Serial baud rate (optional)
            halt_on_error: Pattern to halt on error
            halt_on_success: Pattern to halt on success
            expect: Expected pattern to check
            timeout: Maximum monitoring time in seconds
            show_timestamp: Whether to prefix output lines with elapsed time
        """
        super().__init__(project_dir, environment, timeout or 3600)
        self.port = port
        self.baud_rate = baud_rate
        self.halt_on_error = halt_on_error
        self.halt_on_success = halt_on_success
        self.expect = expect
        self.monitor_timeout = timeout
        self.show_timestamp = show_timestamp

    def create_request(self) -> MonitorRequest:
        """Create monitor request."""
        return MonitorRequest(
            project_dir=str(self.project_dir.absolute()),
            environment=self.environment,
            port=self.port,
            baud_rate=self.baud_rate,
            halt_on_error=self.halt_on_error,
            halt_on_success=self.halt_on_success,
            expect=self.expect,
            timeout=self.monitor_timeout,
            caller_pid=os.getpid(),
            caller_cwd=os.getcwd(),
            show_timestamp=self.show_timestamp,
        )

    def get_request_file(self) -> Path:
        """Get monitor request file path."""
        return MONITOR_REQUEST_FILE

    def get_operation_name(self) -> str:
        """Get operation name."""
        return "Monitor"

    def get_operation_emoji(self) -> str:
        """Get operation emoji."""
        return "👁️"

    def should_tail_output(self) -> bool:
        """Check if output should be tailed."""
        return True

    def print_submission_info(self) -> None:
        """Print monitor submission information."""
        super().print_submission_info()
        if self.port:
            print(f"   Port: {self.port}")
        if self.baud_rate:
            print(f"   Baud rate: {self.baud_rate}")
        if self.monitor_timeout:
            print(f"   Timeout: {self.monitor_timeout}s")

    def on_completion(self, elapsed: float) -> None:
        """Handle completion with monitor summary."""
        if self.monitoring_started:
            _display_monitor_summary(self.project_dir)

    def on_failure(self, status: DaemonStatus, elapsed: float) -> None:
        """Handle failure with monitor summary."""
        if self.monitoring_started:
            _display_monitor_summary(self.project_dir)


class InstallDependenciesRequestHandler(BaseRequestHandler):
    """Handler for install dependencies requests."""

    def __init__(
        self,
        project_dir: Path,
        environment: str,
        verbose: bool = False,
        timeout: float = 1800,
    ):
        """Initialize install dependencies request handler.

        Args:
            project_dir: Project directory
            environment: Build environment
            verbose: Enable verbose output
            timeout: Maximum wait time in seconds
        """
        super().__init__(project_dir, environment, timeout)
        self.verbose = verbose

    def create_request(self) -> InstallDependenciesRequest:
        """Create install dependencies request."""
        return InstallDependenciesRequest(
            project_dir=str(self.project_dir.absolute()),
            environment=self.environment,
            verbose=self.verbose,
            caller_pid=os.getpid(),
            caller_cwd=os.getcwd(),
        )

    def get_request_file(self) -> Path:
        """Get install dependencies request file path."""
        return INSTALL_DEPS_REQUEST_FILE

    def get_operation_name(self) -> str:
        """Get operation name."""
        return "Install Dependencies"

    def get_operation_emoji(self) -> str:
        """Get operation emoji."""
        return "📦"

    def print_submission_info(self) -> None:
        """Print install dependencies submission information."""
        super().print_submission_info()
        if self.verbose:
            print("   Verbose: Yes")


# ============================================================================
# PUBLIC API FUNCTIONS
# ============================================================================


def request_build(
    project_dir: Path,
    environment: str,
    clean_build: bool = False,
    verbose: bool = False,
    timeout: float = 1800,
    jobs: int | None = None,
) -> bool:
    """Request a build operation from the daemon.

    Args:
        project_dir: Project directory
        environment: Build environment
        clean_build: Whether to perform clean build
        verbose: Enable verbose build output
        timeout: Maximum wait time in seconds (default: 30 minutes)
        jobs: Number of parallel compilation jobs (default: CPU count, 1 for serial)

    Returns:
        True if build successful, False otherwise
    """
    handler = BuildRequestHandler(
        project_dir=project_dir,
        environment=environment,
        clean_build=clean_build,
        verbose=verbose,
        timeout=timeout,
        jobs=jobs,
    )
    return handler.execute()


def request_install_dependencies(
    project_dir: Path,
    environment: str,
    verbose: bool = False,
    timeout: float = 1800,
) -> bool:
    """Request a dependency installation operation from the daemon.

    This pre-installs toolchain, platform, framework, and libraries without
    actually performing a build. Useful for:
    - Pre-warming the cache before builds
    - Ensuring dependencies are available offline
    - Separating dependency installation from compilation

    Args:
        project_dir: Project directory
        environment: Build environment
        verbose: Enable verbose output
        timeout: Maximum wait time in seconds (default: 30 minutes)

    Returns:
        True if dependencies installed successfully, False otherwise
    """
    handler = InstallDependenciesRequestHandler(
        project_dir=project_dir,
        environment=environment,
        verbose=verbose,
        timeout=timeout,
    )
    return handler.execute()


def request_deploy(
    project_dir: Path,
    environment: str,
    port: str | None = None,
    clean_build: bool = False,
    monitor_after: bool = False,
    monitor_timeout: float | None = None,
    monitor_halt_on_error: str | None = None,
    monitor_halt_on_success: str | None = None,
    monitor_expect: str | None = None,
    monitor_show_timestamp: bool = False,
    timeout: float = 1800,
) -> bool:
    """Request a deploy operation from the daemon.

    Args:
        project_dir: Project directory
        environment: Build environment
        port: Serial port (optional, auto-detect if None)
        clean_build: Whether to perform clean build
        monitor_after: Whether to start monitor after deploy
        monitor_timeout: Timeout for monitor (if monitor_after=True)
        monitor_halt_on_error: Pattern to halt on error (if monitor_after=True)
        monitor_halt_on_success: Pattern to halt on success (if monitor_after=True)
        monitor_expect: Expected pattern to check at timeout/success (if monitor_after=True)
        monitor_show_timestamp: Whether to prefix output lines with elapsed time (SS.HH format)
        timeout: Maximum wait time in seconds (default: 30 minutes)

    Returns:
        True if deploy successful, False otherwise
    """
    handler = DeployRequestHandler(
        project_dir=project_dir,
        environment=environment,
        port=port,
        clean_build=clean_build,
        monitor_after=monitor_after,
        monitor_timeout=monitor_timeout,
        monitor_halt_on_error=monitor_halt_on_error,
        monitor_halt_on_success=monitor_halt_on_success,
        monitor_expect=monitor_expect,
        monitor_show_timestamp=monitor_show_timestamp,
        timeout=timeout,
    )
    return handler.execute()


def request_monitor(
    project_dir: Path,
    environment: str,
    port: str | None = None,
    baud_rate: int | None = None,
    halt_on_error: str | None = None,
    halt_on_success: str | None = None,
    expect: str | None = None,
    timeout: float | None = None,
    show_timestamp: bool = False,
) -> bool:
    """Request a monitor operation from the daemon.

    Args:
        project_dir: Project directory
        environment: Build environment
        port: Serial port (optional, auto-detect if None)
        baud_rate: Serial baud rate (optional)
        halt_on_error: Pattern to halt on (error detection)
        halt_on_success: Pattern to halt on (success detection)
        expect: Expected pattern to check at timeout/success
        timeout: Maximum monitoring time in seconds
        show_timestamp: Whether to prefix output lines with elapsed time (SS.HH format)

    Returns:
        True if monitoring successful, False otherwise
    """
    handler = MonitorRequestHandler(
        project_dir=project_dir,
        environment=environment,
        port=port,
        baud_rate=baud_rate,
        halt_on_error=halt_on_error,
        halt_on_success=halt_on_success,
        expect=expect,
        timeout=timeout,
        show_timestamp=show_timestamp,
    )
    return handler.execute()
