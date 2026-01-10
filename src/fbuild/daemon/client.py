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
from pathlib import Path
from typing import Any

import psutil

from fbuild.daemon.messages import (
    BuildRequest,
    DaemonState,
    DaemonStatus,
    DeployRequest,
    MonitorRequest,
)

# Daemon configuration (must match daemon settings)
DAEMON_NAME = "fbuild_daemon"
DAEMON_DIR = Path.home() / ".fbuild" / "daemon"
PID_FILE = DAEMON_DIR / f"{DAEMON_NAME}.pid"
STATUS_FILE = DAEMON_DIR / "daemon_status.json"
BUILD_REQUEST_FILE = DAEMON_DIR / "build_request.json"
DEPLOY_REQUEST_FILE = DAEMON_DIR / "deploy_request.json"
MONITOR_REQUEST_FILE = DAEMON_DIR / "monitor_request.json"


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
    # Ensure daemon is running
    if not ensure_daemon_running():
        return False

    print("\n📤 Submitting build request...")
    print(f"   Project: {project_dir}")
    print(f"   Environment: {environment}")
    if clean_build:
        print("   Clean build: Yes")

    # Create request
    request = BuildRequest(
        project_dir=str(project_dir.absolute()),
        environment=environment,
        clean_build=clean_build,
        verbose=verbose,
        caller_pid=os.getpid(),
        caller_cwd=os.getcwd(),
    )

    # Submit request
    write_request_file(BUILD_REQUEST_FILE, request)
    print(f"   Request ID: {request.request_id}")
    print("   ✅ Submitted\n")

    # Monitor progress
    print("🔨 Build Progress:")
    start_time = time.time()
    last_message: str | None = None

    while True:
        try:
            elapsed = time.time() - start_time

            # Check timeout
            if elapsed > timeout:
                print(f"\n❌ Build timeout ({timeout}s)")
                return False

            # Read status
            status = read_status_file()

            # Display progress when message changes
            if status.message != last_message:
                display_status(status)
                last_message = status.message

            # Check completion
            if status.state == DaemonState.COMPLETED:
                print(f"\n✅ Build completed in {elapsed:.1f}s")
                return True
            elif status.state == DaemonState.FAILED:
                print(f"\n❌ Build failed: {status.message}")
                return False

            # Sleep before next poll
            time.sleep(0.5)

        except KeyboardInterrupt:  # noqa: KBI002
            # Prompt user whether to keep the operation running
            print("\n\n⚠️  Interrupted by user (Ctrl-C)")
            response = input("Keep operation running in background? (y/n): ").strip().lower()

            if response in ("y", "yes"):
                print("\n✅ Operation continues in background")
                print("   Check status: fbuild daemon status")
                print("   Stop daemon: fbuild daemon stop")
                return False  # Operation not completed, but detached
            else:
                print("\n🛑 Requesting daemon to stop operation...")
                # Create a cancel signal file
                cancel_file = DAEMON_DIR / f"cancel_{request.request_id}.signal"
                cancel_file.touch()
                print("   Operation cancellation requested")
                return False


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
        timeout: Maximum wait time in seconds (default: 30 minutes)

    Returns:
        True if deploy successful, False otherwise
    """
    # Ensure daemon is running
    if not ensure_daemon_running():
        return False

    print("\n📤 Submitting deploy request...")
    print(f"   Project: {project_dir}")
    print(f"   Environment: {environment}")
    if port:
        print(f"   Port: {port}")

    # Create request
    request = DeployRequest(
        project_dir=str(project_dir.absolute()),
        environment=environment,
        port=port,
        clean_build=clean_build,
        monitor_after=monitor_after,
        monitor_timeout=monitor_timeout,
        monitor_halt_on_error=monitor_halt_on_error,
        monitor_halt_on_success=monitor_halt_on_success,
        monitor_expect=monitor_expect,
        caller_pid=os.getpid(),
        caller_cwd=os.getcwd(),
    )

    # Submit request
    write_request_file(DEPLOY_REQUEST_FILE, request)
    print(f"   Request ID: {request.request_id}")
    print("   ✅ Submitted\n")

    # Monitor progress
    print("📦 Deploy Progress:")
    start_time = time.time()
    last_message: str | None = None

    while True:
        try:
            elapsed = time.time() - start_time

            # Check timeout
            if elapsed > timeout:
                print(f"\n❌ Deploy timeout ({timeout}s)")
                return False

            # Read status
            status = read_status_file()

            # Display progress when message changes
            if status.message != last_message:
                display_status(status)
                last_message = status.message

            # Check completion
            if status.state == DaemonState.COMPLETED:
                print(f"\n✅ Deploy completed in {elapsed:.1f}s")
                return True
            elif status.state == DaemonState.FAILED:
                print(f"\n❌ Deploy failed: {status.message}")
                return False

            # Sleep before next poll
            time.sleep(0.5)

        except KeyboardInterrupt:  # noqa: KBI002
            # Prompt user whether to keep the operation running
            print("\n\n⚠️  Interrupted by user (Ctrl-C)")
            response = input("Keep operation running in background? (y/n): ").strip().lower()

            if response in ("y", "yes"):
                print("\n✅ Operation continues in background")
                print("   Check status: fbuild daemon status")
                print("   Stop daemon: fbuild daemon stop")
                return False  # Operation not completed, but detached
            else:
                print("\n🛑 Requesting daemon to stop operation...")
                # Create a cancel signal file
                cancel_file = DAEMON_DIR / f"cancel_{request.request_id}.signal"
                cancel_file.touch()
                print("   Operation cancellation requested")
                return False


def request_monitor(
    project_dir: Path,
    environment: str,
    port: str | None = None,
    baud_rate: int | None = None,
    halt_on_error: str | None = None,
    halt_on_success: str | None = None,
    expect: str | None = None,
    timeout: float | None = None,
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

    Returns:
        True if monitoring successful, False otherwise
    """
    # Ensure daemon is running
    if not ensure_daemon_running():
        return False

    print("\n📤 Submitting monitor request...")
    print(f"   Project: {project_dir}")
    print(f"   Environment: {environment}")
    if port:
        print(f"   Port: {port}")
    if baud_rate:
        print(f"   Baud rate: {baud_rate}")
    if timeout:
        print(f"   Timeout: {timeout}s")

    # Create request
    request = MonitorRequest(
        project_dir=str(project_dir.absolute()),
        environment=environment,
        port=port,
        baud_rate=baud_rate,
        halt_on_error=halt_on_error,
        halt_on_success=halt_on_success,
        expect=expect,
        timeout=timeout,
        caller_pid=os.getpid(),
        caller_cwd=os.getcwd(),
    )

    # Submit request
    write_request_file(MONITOR_REQUEST_FILE, request)
    print(f"   Request ID: {request.request_id}")
    print("   ✅ Submitted\n")

    # Monitor progress
    print("👁️  Monitor Output:")
    start_time = time.time()
    last_message: str | None = None

    while True:
        try:
            elapsed = time.time() - start_time

            # Check timeout
            if timeout and elapsed > timeout:
                print(f"\n⏱️  Monitor timeout ({timeout}s)")
                return True  # Timeout is expected for monitor

            # Read status
            status = read_status_file()

            # Display progress when message changes
            if status.message != last_message:
                display_status(status)
                last_message = status.message

                # Display recent output lines
                if status.output_lines:
                    for line in status.output_lines:
                        print(f"     {line}")

            # Check completion
            if status.state == DaemonState.COMPLETED:
                print(f"\n✅ Monitor completed in {elapsed:.1f}s")
                return True
            elif status.state == DaemonState.FAILED:
                print(f"\n❌ Monitor failed: {status.message}")
                return False

            # Sleep before next poll
            time.sleep(0.5)

        except KeyboardInterrupt:  # noqa: KBI002
            # Prompt user whether to keep the operation running
            print("\n\n⚠️  Interrupted by user (Ctrl-C)")
            response = input("Keep operation running in background? (y/n): ").strip().lower()

            if response in ("y", "yes"):
                print("\n✅ Operation continues in background")
                print("   Check status: fbuild daemon status")
                print("   Stop daemon: fbuild daemon stop")
                return False  # Operation not completed, but detached
            else:
                print("\n🛑 Requesting daemon to stop operation...")
                # Create a cancel signal file
                cancel_file = DAEMON_DIR / f"cancel_{request.request_id}.signal"
                cancel_file.touch()
                print("   Operation cancellation requested")
                return False


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


def main() -> int:
    """Command-line interface for client."""
    import argparse

    parser = argparse.ArgumentParser(description="fbuild Daemon Client")
    parser.add_argument("--status", action="store_true", help="Show daemon status")
    parser.add_argument("--stop", action="store_true", help="Stop the daemon")

    args = parser.parse_args()

    if args.status:
        status = get_daemon_status()
        print("Daemon Status:")
        print(json.dumps(status, indent=2))
        return 0

    if args.stop:
        return 0 if stop_daemon() else 1

    parser.print_help()
    return 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:  # noqa: KBI002
        print("\nInterrupted by user")
        sys.exit(130)
