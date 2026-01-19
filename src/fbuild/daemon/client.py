"""
fbuild Daemon Client

Client interface for requesting deploy and monitor operations from the daemon.
Handles daemon lifecycle, request submission, and progress monitoring.
"""

import _thread
import json
import os
import subprocess
import sys
import time
from abc import ABC, abstractmethod
from pathlib import Path
from typing import Any

import psutil

from fbuild.daemon.messages import (
    BuildRequest,
    DaemonState,
    DaemonStatus,
    DeployRequest,
    InstallDependenciesRequest,
    MonitorRequest,
)

# Spinner characters for progress indication
SPINNER_CHARS = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]

# Daemon configuration (must match daemon settings)
DAEMON_NAME = "fbuild_daemon"

# Check for development mode (when running from repo)
if os.environ.get("FBUILD_DEV_MODE") == "1":
    # Use project-local daemon directory for development
    DAEMON_DIR = Path.cwd() / ".fbuild" / "daemon_dev"
else:
    # Use home directory for production
    DAEMON_DIR = Path.home() / ".fbuild" / "daemon"

PID_FILE = DAEMON_DIR / f"{DAEMON_NAME}.pid"
STATUS_FILE = DAEMON_DIR / "daemon_status.json"
BUILD_REQUEST_FILE = DAEMON_DIR / "build_request.json"
DEPLOY_REQUEST_FILE = DAEMON_DIR / "deploy_request.json"
MONITOR_REQUEST_FILE = DAEMON_DIR / "monitor_request.json"
INSTALL_DEPS_REQUEST_FILE = DAEMON_DIR / "install_deps_request.json"


def is_daemon_running() -> bool:
    """Check if daemon is running, clean up stale PID files.

    Returns:
        True if daemon is running, False otherwise
    """
    if not PID_FILE.exists():
        return False

    try:
        with open(PID_FILE) as f:
            pid = int(f.read().strip())

        # Check if process exists
        if psutil.pid_exists(pid):
            return True
        else:
            # Stale PID file - remove it
            print(f"Removing stale PID file: {PID_FILE}")
            PID_FILE.unlink()
            return False
    except KeyboardInterrupt:
        _thread.interrupt_main()
        raise
    except Exception:
        # Corrupted PID file - remove it
        try:
            PID_FILE.unlink(missing_ok=True)
        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception:
            pass
        return False


def start_daemon() -> None:
    """Start the daemon process."""
    daemon_script = Path(__file__).parent / "daemon.py"

    if not daemon_script.exists():
        raise RuntimeError(f"Daemon script not found: {daemon_script}")

    # Start daemon in background
    subprocess.Popen(
        [sys.executable, str(daemon_script)],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        stdin=subprocess.DEVNULL,
    )


def read_status_file() -> DaemonStatus:
    """Read current daemon status with corruption recovery.

    Returns:
        DaemonStatus object (or default status if file doesn't exist or corrupted)
    """
    if not STATUS_FILE.exists():
        return DaemonStatus(
            state=DaemonState.UNKNOWN,
            message="Status file not found",
            updated_at=time.time(),
        )

    try:
        with open(STATUS_FILE) as f:
            data = json.load(f)

        # Parse into typed DaemonStatus
        return DaemonStatus.from_dict(data)

    except (json.JSONDecodeError, ValueError):
        # Corrupted JSON - return default status
        return DaemonStatus(
            state=DaemonState.UNKNOWN,
            message="Status file corrupted (invalid JSON)",
            updated_at=time.time(),
        )
    except KeyboardInterrupt:
        _thread.interrupt_main()
        raise
    except Exception:
        return DaemonStatus(
            state=DaemonState.UNKNOWN,
            message="Failed to read status",
            updated_at=time.time(),
        )


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


def display_status(status: DaemonStatus, prefix: str = "  ") -> None:
    """Display status update to user.

    Args:
        status: DaemonStatus object
        prefix: Line prefix for indentation
    """
    # Show current operation if available, otherwise use message
    display_text = status.current_operation or status.message

    if status.state == DaemonState.DEPLOYING:
        print(f"{prefix}📦 {display_text}", flush=True)
    elif status.state == DaemonState.MONITORING:
        print(f"{prefix}👁️  {display_text}", flush=True)
    elif status.state == DaemonState.BUILDING:
        print(f"{prefix}🔨 {display_text}", flush=True)
    elif status.state == DaemonState.COMPLETED:
        print(f"{prefix}✅ {display_text}", flush=True)
    elif status.state == DaemonState.FAILED:
        print(f"{prefix}❌ {display_text}", flush=True)
    else:
        print(f"{prefix}ℹ️  {display_text}", flush=True)


def display_spinner_progress(
    status: DaemonStatus,
    elapsed: float,
    spinner_idx: int,
    prefix: str = "  ",
) -> None:
    """Display spinner with elapsed time when status hasn't changed.

    Uses carriage return to update in place without new line.

    Args:
        status: DaemonStatus object
        elapsed: Elapsed time in seconds
        spinner_idx: Current spinner index
        prefix: Line prefix for indentation
    """
    spinner = SPINNER_CHARS[spinner_idx % len(SPINNER_CHARS)]
    display_text = status.current_operation or status.message

    # Format elapsed time
    mins = int(elapsed) // 60
    secs = int(elapsed) % 60
    if mins > 0:
        time_str = f"{mins}m {secs}s"
    else:
        time_str = f"{secs}s"

    # Use carriage return to update in place
    print(f"\r{prefix}{spinner} {display_text} ({time_str})", end="", flush=True)


def ensure_daemon_running() -> bool:
    """Ensure daemon is running, start if needed.

    Returns:
        True if daemon is running or started successfully, False otherwise
    """
    if is_daemon_running():
        return True

    # If we reach here, daemon is not running (stale PID was cleaned by is_daemon_running)
    # Clear stale status file to prevent race condition where client reads old status
    # from previous daemon run before new daemon writes fresh status
    if STATUS_FILE.exists():
        try:
            STATUS_FILE.unlink()
        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception:
            pass  # Best effort - continue even if delete fails

    print("🔗 Starting fbuild daemon...")
    start_daemon()

    # Wait up to 10 seconds for daemon to start and write fresh status
    for _ in range(10):
        if is_daemon_running():
            # Daemon is running - check if status file is fresh
            status = read_status_file()
            if status.state != DaemonState.UNKNOWN:
                # Valid status received from new daemon
                print("✅ Daemon started successfully")
                return True
        time.sleep(1)

    print("❌ Failed to start daemon")
    return False


def request_build(
    project_dir: Path,
    environment: str,
    clean_build: bool = False,
    verbose: bool = False,
    timeout: float = 1800,
) -> bool:
    """Request a build operation from the daemon.

    Args:
        project_dir: Project directory
        environment: Build environment
        clean_build: Whether to perform clean build
        verbose: Enable verbose build output
        timeout: Maximum wait time in seconds (default: 30 minutes)

    Returns:
        True if build successful, False otherwise
    """
    handler = BuildRequestHandler(
        project_dir=project_dir,
        environment=environment,
        clean_build=clean_build,
        verbose=verbose,
        timeout=timeout,
    )
    return handler.execute()


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

    except KeyboardInterrupt:  # noqa: KBI002
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
        self.output_file_position = 0
        self.spinner_idx = 0
        self.last_spinner_update = 0.0

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
        output_file = self.project_dir / ".fbuild" / "monitor_output.txt"
        if output_file.exists():
            try:
                with open(output_file, "r", encoding="utf-8", errors="replace") as f:
                    f.seek(self.output_file_position)
                    new_lines = f.read()
                    if new_lines:
                        print(new_lines, end="", flush=True)
                        self.output_file_position = f.tell()
            except KeyboardInterrupt:  # noqa: KBI002
                raise
            except Exception:
                pass  # Ignore read errors

    def read_remaining_output(self) -> None:
        """Read any remaining output from output file."""
        if not self.monitoring_started:
            return

        output_file = self.project_dir / ".fbuild" / "monitor_output.txt"
        if output_file.exists():
            try:
                with open(output_file, "r", encoding="utf-8", errors="replace") as f:
                    f.seek(self.output_file_position)
                    new_lines = f.read()
                    if new_lines:
                        print(new_lines, end="", flush=True)
            except KeyboardInterrupt:  # noqa: KBI002
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
            print("   Operation cancellation requested")
            return False

    def execute(self) -> bool:
        """Execute the request and monitor progress.

        Returns:
            True if operation successful, False otherwise
        """
        # Ensure daemon is running
        if not ensure_daemon_running():
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
                    if status.state in (DaemonState.BUILDING, DaemonState.DEPLOYING):
                        current_time = time.time()
                        # Update spinner every 100ms
                        if current_time - self.last_spinner_update >= 0.1:
                            self.spinner_idx += 1
                            display_spinner_progress(status, elapsed, self.spinner_idx)
                            self.last_spinner_update = current_time

                # Handle monitoring phase
                if self.should_tail_output() and status.state == DaemonState.MONITORING:
                    if not self.monitoring_started:
                        self.monitoring_started = True
                        # Clear spinner line before monitor output
                        print("\r" + " " * 80 + "\r", end="", flush=True)
                        print()  # Blank line before serial output
                        self.on_monitoring_started()

                if self.monitoring_started and self.should_tail_output():
                    self.tail_output_file()

                # Check completion
                if status.state == DaemonState.COMPLETED:
                    if status.request_id == request.request_id:
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

                # Sleep before next poll
                poll_interval = 0.1 if self.monitoring_started else 0.1  # Faster polling for spinner
                time.sleep(poll_interval)

            except KeyboardInterrupt:  # noqa: KBI002
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
    ):
        """Initialize build request handler.

        Args:
            project_dir: Project directory
            environment: Build environment
            clean_build: Whether to perform clean build
            verbose: Enable verbose build output
            timeout: Maximum wait time in seconds
        """
        super().__init__(project_dir, environment, timeout)
        self.clean_build = clean_build
        self.verbose = verbose

    def create_request(self) -> BuildRequest:
        """Create build request."""
        return BuildRequest(
            project_dir=str(self.project_dir.absolute()),
            environment=self.environment,
            clean_build=self.clean_build,
            verbose=self.verbose,
            caller_pid=os.getpid(),
            caller_cwd=os.getcwd(),
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
        return self.monitor_after

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


def stop_daemon() -> bool:
    """Stop the daemon gracefully.

    Returns:
        True if daemon was stopped, False otherwise
    """
    if not is_daemon_running():
        print("Daemon is not running")
        return False

    # Create shutdown signal file
    shutdown_file = DAEMON_DIR / "shutdown.signal"
    shutdown_file.touch()

    # Wait for daemon to exit
    print("Stopping daemon...")
    for _ in range(10):
        if not is_daemon_running():
            print("✅ Daemon stopped")
            return True
        time.sleep(1)

    print("⚠️  Daemon did not stop gracefully")
    return False


def get_daemon_status() -> dict[str, Any]:
    """Get current daemon status.

    Returns:
        Dictionary with daemon status information
    """
    status: dict[str, Any] = {
        "running": is_daemon_running(),
        "pid_file_exists": PID_FILE.exists(),
        "status_file_exists": STATUS_FILE.exists(),
    }

    if PID_FILE.exists():
        try:
            with open(PID_FILE) as f:
                status["pid"] = int(f.read().strip())
        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception:
            status["pid"] = None

    if STATUS_FILE.exists():
        daemon_status = read_status_file()
        # Convert DaemonStatus to dict for JSON serialization
        status["current_status"] = daemon_status.to_dict()

    return status


def get_lock_status() -> dict[str, Any]:
    """Get current lock status from the daemon.

    Reads the daemon status file and extracts lock information.
    This shows which ports and projects have active locks and who holds them.

    Returns:
        Dictionary with lock status information:
        - port_locks: Dict of port -> lock info
        - project_locks: Dict of project -> lock info
        - stale_locks: List of locks that appear to be stale

    Example:
        >>> status = get_lock_status()
        >>> for port, info in status["port_locks"].items():
        ...     if info.get("is_held"):
        ...         print(f"Port {port} locked by: {info.get('holder_description')}")
    """
    status = read_status_file()
    locks = status.locks if hasattr(status, "locks") and status.locks else {}

    # Extract stale locks
    stale_locks: list[dict[str, Any]] = []

    port_locks = locks.get("port_locks", {})
    for port, info in port_locks.items():
        if isinstance(info, dict) and info.get("is_stale"):
            stale_locks.append(
                {
                    "type": "port",
                    "resource": port,
                    "holder": info.get("holder_description"),
                    "hold_duration": info.get("hold_duration"),
                }
            )

    project_locks = locks.get("project_locks", {})
    for project, info in project_locks.items():
        if isinstance(info, dict) and info.get("is_stale"):
            stale_locks.append(
                {
                    "type": "project",
                    "resource": project,
                    "holder": info.get("holder_description"),
                    "hold_duration": info.get("hold_duration"),
                }
            )

    return {
        "port_locks": port_locks,
        "project_locks": project_locks,
        "stale_locks": stale_locks,
    }


def request_clear_stale_locks() -> bool:
    """Request the daemon to clear stale locks.

    Sends a signal to the daemon to force-release any locks that have been
    held beyond their timeout. This is useful when operations have hung or
    crashed without properly releasing their locks.

    Returns:
        True if signal was sent, False if daemon not running

    Note:
        The daemon checks for stale locks periodically (every 60 seconds).
        This function triggers an immediate check by writing a signal file.
        The actual clearing happens on the daemon side.
    """
    if not is_daemon_running():
        print("Daemon is not running")
        return False

    # Create signal file to trigger stale lock cleanup
    signal_file = DAEMON_DIR / "clear_stale_locks.signal"
    DAEMON_DIR.mkdir(parents=True, exist_ok=True)
    signal_file.touch()

    print("Signal sent to daemon to clear stale locks")
    print("Note: Check status with 'fbuild daemon status' to see results")
    return True


def display_lock_status() -> None:
    """Display current lock status in a human-readable format."""
    if not is_daemon_running():
        print("Daemon is not running - no active locks")
        return

    lock_status = get_lock_status()

    print("\n=== Lock Status ===\n")

    # Port locks
    port_locks = lock_status.get("port_locks", {})
    if port_locks:
        print("Port Locks:")
        for port, info in port_locks.items():
            if isinstance(info, dict):
                held = info.get("is_held", False)
                stale = info.get("is_stale", False)
                holder = info.get("holder_description", "unknown")
                duration = info.get("hold_duration")

                status_str = "FREE"
                if held:
                    status_str = "STALE" if stale else "HELD"

                duration_str = f" ({duration:.1f}s)" if duration else ""
                holder_str = f" by {holder}" if held else ""

                print(f"  {port}: {status_str}{holder_str}{duration_str}")
    else:
        print("Port Locks: (none)")

    # Project locks
    project_locks = lock_status.get("project_locks", {})
    if project_locks:
        print("\nProject Locks:")
        for project, info in project_locks.items():
            if isinstance(info, dict):
                held = info.get("is_held", False)
                stale = info.get("is_stale", False)
                holder = info.get("holder_description", "unknown")
                duration = info.get("hold_duration")

                status_str = "FREE"
                if held:
                    status_str = "STALE" if stale else "HELD"

                duration_str = f" ({duration:.1f}s)" if duration else ""
                holder_str = f" by {holder}" if held else ""

                # Truncate long project paths
                display_project = project
                if len(project) > 50:
                    display_project = "..." + project[-47:]

                print(f"  {display_project}: {status_str}{holder_str}{duration_str}")
    else:
        print("\nProject Locks: (none)")

    # Stale locks warning
    stale_locks = lock_status.get("stale_locks", [])
    if stale_locks:
        print(f"\n⚠️  Found {len(stale_locks)} stale lock(s)!")
        print("   Use 'fbuild daemon clear-locks' to force-release them")

    print()


def list_all_daemons() -> list[dict[str, Any]]:
    """List all running fbuild daemon instances by scanning processes.

    This function scans all running processes to find fbuild daemons,
    which is useful for detecting multiple daemon instances that may
    have been started due to race conditions or startup errors.

    Returns:
        List of dictionaries with daemon info:
        - pid: Process ID
        - cmdline: Command line arguments
        - uptime: Time since process started (seconds)
        - is_primary: True if this matches the PID file (primary daemon)

    Example:
        >>> daemons = list_all_daemons()
        >>> for d in daemons:
        ...     print(f"PID {d['pid']}: uptime {d['uptime']:.1f}s")
    """
    daemons: list[dict[str, Any]] = []

    # Get primary daemon PID from PID file
    primary_pid = None
    if PID_FILE.exists():
        try:
            with open(PID_FILE) as f:
                primary_pid = int(f.read().strip())
        except (ValueError, OSError):
            pass

    for proc in psutil.process_iter(["pid", "cmdline", "create_time", "name"]):
        try:
            cmdline = proc.info.get("cmdline")
            proc_name = proc.info.get("name", "")
            if not cmdline:
                continue

            # Skip non-Python processes
            if not proc_name.lower().startswith("python"):
                continue

            # Detect fbuild daemon processes
            # Look for patterns like "python daemon.py" in fbuild package
            is_daemon = False

            # Check for direct daemon.py execution from fbuild package
            # Must end with daemon.py and have fbuild in the path
            for arg in cmdline:
                if arg.endswith("daemon.py") and "fbuild" in arg.lower():
                    is_daemon = True
                    break

            # Check for python -m fbuild.daemon.daemon execution
            if not is_daemon and "-m" in cmdline:
                for i, arg in enumerate(cmdline):
                    if arg == "-m" and i + 1 < len(cmdline):
                        module = cmdline[i + 1]
                        if module in ("fbuild.daemon.daemon", "fbuild.daemon"):
                            is_daemon = True
                            break

            if is_daemon:
                pid = proc.info["pid"]
                create_time = proc.info.get("create_time", time.time())
                daemons.append(
                    {
                        "pid": pid,
                        "cmdline": cmdline,
                        "uptime": time.time() - create_time,
                        "is_primary": pid == primary_pid,
                    }
                )

        except (psutil.NoSuchProcess, psutil.AccessDenied, psutil.ZombieProcess):
            continue

    return daemons


def force_kill_daemon(pid: int) -> bool:
    """Force kill a daemon process by PID using SIGKILL.

    This is a forceful termination that doesn't give the daemon
    time to clean up. Use graceful_kill_daemon() when possible.

    Args:
        pid: Process ID to kill

    Returns:
        True if process was killed, False if it didn't exist

    Example:
        >>> if force_kill_daemon(12345):
        ...     print("Daemon killed")
    """
    try:
        proc = psutil.Process(pid)
        proc.kill()  # SIGKILL on Unix, TerminateProcess on Windows
        proc.wait(timeout=5)
        return True
    except psutil.NoSuchProcess:
        return False
    except psutil.TimeoutExpired:
        # Process didn't die even with SIGKILL - unusual but handle it
        return True
    except psutil.AccessDenied:
        print(f"Access denied: cannot kill process {pid}")
        return False


def graceful_kill_daemon(pid: int, timeout: int = 10) -> bool:
    """Gracefully terminate a daemon process with fallback to force kill.

    Sends SIGTERM first to allow cleanup, then SIGKILL if the process
    doesn't exit within the timeout period.

    Args:
        pid: Process ID to terminate
        timeout: Seconds to wait before force killing (default: 10)

    Returns:
        True if process was terminated, False if it didn't exist

    Example:
        >>> if graceful_kill_daemon(12345, timeout=5):
        ...     print("Daemon terminated gracefully")
    """
    try:
        proc = psutil.Process(pid)
        proc.terminate()  # SIGTERM on Unix, TerminateProcess on Windows

        try:
            proc.wait(timeout=timeout)
            return True
        except psutil.TimeoutExpired:
            # Process didn't exit gracefully - force kill
            print(f"Process {pid} didn't exit gracefully, force killing...")
            proc.kill()
            proc.wait(timeout=5)
            return True

    except psutil.NoSuchProcess:
        return False
    except psutil.AccessDenied:
        print(f"Access denied: cannot terminate process {pid}")
        return False


def kill_all_daemons(force: bool = False) -> int:
    """Kill all running daemon instances.

    Useful when multiple daemons have started due to race conditions
    or when the daemon system is in an inconsistent state.

    Args:
        force: If True, use SIGKILL immediately. If False, try SIGTERM first.

    Returns:
        Number of daemons killed

    Example:
        >>> killed = kill_all_daemons(force=False)
        >>> print(f"Killed {killed} daemon(s)")
    """
    killed = 0
    daemons = list_all_daemons()

    if not daemons:
        return 0

    for daemon in daemons:
        pid = daemon["pid"]
        if force:
            if force_kill_daemon(pid):
                killed += 1
                print(f"Force killed daemon (PID {pid})")
        else:
            if graceful_kill_daemon(pid):
                killed += 1
                print(f"Gracefully terminated daemon (PID {pid})")

    # Clean up PID file if we killed any daemons
    if killed > 0 and PID_FILE.exists():
        try:
            PID_FILE.unlink()
        except OSError:
            pass

    return killed


def display_daemon_list() -> None:
    """Display all running daemon instances in a human-readable format."""
    daemons = list_all_daemons()

    if not daemons:
        print("No fbuild daemon instances found")
        return

    print(f"\n=== Running fbuild Daemons ({len(daemons)} found) ===\n")

    for daemon in daemons:
        pid = daemon["pid"]
        uptime = daemon["uptime"]
        is_primary = daemon["is_primary"]

        # Format uptime
        if uptime < 60:
            uptime_str = f"{uptime:.1f}s"
        elif uptime < 3600:
            uptime_str = f"{uptime / 60:.1f}m"
        else:
            uptime_str = f"{uptime / 3600:.1f}h"

        primary_str = " (PRIMARY)" if is_primary else " (ORPHAN)"
        print(f"  PID {pid}: uptime {uptime_str}{primary_str}")

    print()

    # Warn about multiple daemons
    if len(daemons) > 1:
        print("⚠️  Multiple daemon instances detected!")
        print("   This can cause lock conflicts and unexpected behavior.")
        print("   Use 'fbuild daemon kill-all' to clean up, then restart.")
        print()


# ============================================================================
# DEVICE MANAGEMENT FUNCTIONS
# ============================================================================


def list_devices(refresh: bool = False) -> list[dict[str, Any]] | None:
    """List all devices known to the daemon.

    Args:
        refresh: Whether to refresh device discovery before listing.

    Returns:
        List of device info dictionaries, or None if daemon not running.
        Each device dict contains:
        - device_id: Stable device identifier
        - port: Current port (may change)
        - is_connected: Whether device is currently connected
        - exclusive_holder: Client ID holding exclusive lease (or None)
        - monitor_count: Number of active monitor leases
    """
    if not is_daemon_running():
        return None

    # For now, we use a signal file to communicate with the daemon
    # In the future, this should use the async TCP connection
    request_file = DAEMON_DIR / "device_list_request.json"
    response_file = DAEMON_DIR / "device_list_response.json"

    # Clean up any old response file
    response_file.unlink(missing_ok=True)

    # Write request
    request = {"refresh": refresh, "timestamp": time.time()}
    with open(request_file, "w") as f:
        json.dump(request, f)

    # Wait for response (timeout 5 seconds)
    for _ in range(50):
        if response_file.exists():
            try:
                with open(response_file) as f:
                    response = json.load(f)
                response_file.unlink(missing_ok=True)
                if response.get("success"):
                    return response.get("devices", [])
                return []
            except (json.JSONDecodeError, OSError):
                pass
        time.sleep(0.1)

    # Timeout - clean up
    request_file.unlink(missing_ok=True)
    return None


def get_device_status(device_id: str) -> dict[str, Any] | None:
    """Get detailed status for a specific device.

    Args:
        device_id: The device ID to query.

    Returns:
        Device status dictionary, or None if device not found or daemon not running.
    """
    if not is_daemon_running():
        return None

    request_file = DAEMON_DIR / "device_status_request.json"
    response_file = DAEMON_DIR / "device_status_response.json"

    # Clean up any old response file
    response_file.unlink(missing_ok=True)

    # Write request
    request = {"device_id": device_id, "timestamp": time.time()}
    with open(request_file, "w") as f:
        json.dump(request, f)

    # Wait for response
    for _ in range(50):
        if response_file.exists():
            try:
                with open(response_file) as f:
                    response = json.load(f)
                response_file.unlink(missing_ok=True)
                if response.get("success"):
                    return response
                return None
            except (json.JSONDecodeError, OSError):
                pass
        time.sleep(0.1)

    request_file.unlink(missing_ok=True)
    return None


def acquire_device_lease(
    device_id: str,
    lease_type: str = "exclusive",
    description: str = "",
) -> dict[str, Any] | None:
    """Acquire a lease on a device.

    Args:
        device_id: The device ID to lease.
        lease_type: Type of lease - "exclusive" or "monitor".
        description: Description of the operation.

    Returns:
        Response dictionary with success status and lease_id, or None if failed.
    """
    if not is_daemon_running():
        return None

    request_file = DAEMON_DIR / "device_lease_request.json"
    response_file = DAEMON_DIR / "device_lease_response.json"

    response_file.unlink(missing_ok=True)

    request = {
        "device_id": device_id,
        "lease_type": lease_type,
        "description": description,
        "timestamp": time.time(),
    }
    with open(request_file, "w") as f:
        json.dump(request, f)

    for _ in range(50):
        if response_file.exists():
            try:
                with open(response_file) as f:
                    response = json.load(f)
                response_file.unlink(missing_ok=True)
                return response
            except (json.JSONDecodeError, OSError):
                pass
        time.sleep(0.1)

    request_file.unlink(missing_ok=True)
    return None


def release_device_lease(device_id: str) -> dict[str, Any] | None:
    """Release a lease on a device.

    Args:
        device_id: The device ID or lease ID to release.

    Returns:
        Response dictionary with success status, or None if failed.
    """
    if not is_daemon_running():
        return None

    request_file = DAEMON_DIR / "device_release_request.json"
    response_file = DAEMON_DIR / "device_release_response.json"

    response_file.unlink(missing_ok=True)

    request = {"device_id": device_id, "timestamp": time.time()}
    with open(request_file, "w") as f:
        json.dump(request, f)

    for _ in range(50):
        if response_file.exists():
            try:
                with open(response_file) as f:
                    response = json.load(f)
                response_file.unlink(missing_ok=True)
                return response
            except (json.JSONDecodeError, OSError):
                pass
        time.sleep(0.1)

    request_file.unlink(missing_ok=True)
    return None


def preempt_device(device_id: str, reason: str) -> dict[str, Any] | None:
    """Preempt a device from its current holder.

    Args:
        device_id: The device ID to preempt.
        reason: Reason for preemption (required).

    Returns:
        Response dictionary with success status and preempted_client_id, or None if failed.
    """
    if not is_daemon_running():
        return None

    if not reason:
        return {"success": False, "message": "Reason is required for preemption"}

    request_file = DAEMON_DIR / "device_preempt_request.json"
    response_file = DAEMON_DIR / "device_preempt_response.json"

    response_file.unlink(missing_ok=True)

    request = {"device_id": device_id, "reason": reason, "timestamp": time.time()}
    with open(request_file, "w") as f:
        json.dump(request, f)

    for _ in range(50):
        if response_file.exists():
            try:
                with open(response_file) as f:
                    response = json.load(f)
                response_file.unlink(missing_ok=True)
                return response
            except (json.JSONDecodeError, OSError):
                pass
        time.sleep(0.1)

    request_file.unlink(missing_ok=True)
    return None


def tail_daemon_logs(follow: bool = True, lines: int = 50) -> None:
    """Tail the daemon log file.

    This function streams the daemon's log output, allowing users to see
    what the daemon is doing in real-time without affecting its operation.

    Per TASK.md: `fbuild show daemon` should attach to daemon log stream
    and tail it, with exit NOT stopping the daemon.

    Args:
        follow: If True, continuously follow the log file (like tail -f).
                If False, just print the last N lines and exit.
        lines: Number of lines to show initially (default: 50).
    """
    log_file = DAEMON_DIR / "daemon.log"

    if not log_file.exists():
        print("❌ Daemon log file not found")
        print(f"   Expected at: {log_file}")
        print("   Hint: Start the daemon first with 'fbuild build <project>'")
        return

    print(f"📋 Tailing daemon log: {log_file}")
    if follow:
        print("   (Press Ctrl-C to stop viewing - daemon will continue running)\n")
    print("=" * 60)

    try:
        with open(log_file, "r", encoding="utf-8", errors="replace") as f:
            # Read initial lines
            all_lines = f.readlines()

            # Show last N lines
            if len(all_lines) > lines:
                print(f"... (showing last {lines} lines) ...\n")
                for line in all_lines[-lines:]:
                    print(line, end="")
            else:
                for line in all_lines:
                    print(line, end="")

            if not follow:
                return

            # Follow mode - continuously read new content
            while True:
                line = f.readline()
                if line:
                    print(line, end="", flush=True)
                else:
                    # No new content - sleep briefly
                    time.sleep(0.1)

    except KeyboardInterrupt:
        import _thread

        _thread.interrupt_main()
        print("\n\n" + "=" * 60)
        print("✅ Stopped viewing logs (daemon continues running)")
        print("   Use 'fbuild daemon status' to check daemon status")
        print("   Use 'fbuild daemon stop' to stop the daemon")


def get_daemon_log_path() -> Path:
    """Get the path to the daemon log file.

    Returns:
        Path to daemon.log file
    """
    return DAEMON_DIR / "daemon.log"


def main() -> int:
    """Command-line interface for client."""
    import argparse

    parser = argparse.ArgumentParser(description="fbuild Daemon Client")
    parser.add_argument("--status", action="store_true", help="Show daemon status")
    parser.add_argument("--stop", action="store_true", help="Stop the daemon")
    parser.add_argument("--locks", action="store_true", help="Show lock status")
    parser.add_argument("--clear-locks", action="store_true", help="Clear stale locks")
    parser.add_argument("--list", action="store_true", help="List all daemon instances")
    parser.add_argument("--kill", type=int, metavar="PID", help="Kill specific daemon by PID")
    parser.add_argument("--kill-all", action="store_true", help="Kill all daemon instances")
    parser.add_argument("--force", action="store_true", help="Force kill (with --kill or --kill-all)")
    parser.add_argument("--tail", action="store_true", help="Tail daemon logs")
    parser.add_argument("--no-follow", action="store_true", help="Don't follow log file (with --tail)")
    parser.add_argument("--lines", type=int, default=50, help="Number of lines to show initially (with --tail)")

    args = parser.parse_args()

    if args.status:
        status = get_daemon_status()
        print("Daemon Status:")
        print(json.dumps(status, indent=2))
        return 0

    if args.stop:
        return 0 if stop_daemon() else 1

    if args.locks:
        display_lock_status()
        return 0

    if args.clear_locks:
        return 0 if request_clear_stale_locks() else 1

    if args.list:
        display_daemon_list()
        return 0

    if args.kill:
        if args.force:
            success = force_kill_daemon(args.kill)
        else:
            success = graceful_kill_daemon(args.kill)
        if success:
            print(f"Daemon (PID {args.kill}) terminated")
            return 0
        else:
            print(f"Failed to terminate daemon (PID {args.kill}) - process may not exist")
            return 1

    if args.kill_all:
        killed = kill_all_daemons(force=args.force)
        print(f"Killed {killed} daemon instance(s)")
        return 0

    if args.tail:
        tail_daemon_logs(follow=not args.no_follow, lines=args.lines)
        return 0

    parser.print_help()
    return 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:  # noqa: KBI002
        print("\nInterrupted by user")
        sys.exit(130)
