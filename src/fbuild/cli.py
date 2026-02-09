"""
Command-line interface for fbuild.

This module provides the `fbuild` CLI tool for building embedded firmware.
"""

import argparse
import os
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

# Auto-detect development mode BEFORE any fbuild imports
# This must happen before importing anything from fbuild because paths.py reads FBUILD_DEV_MODE at import time
_dev_mode_auto_detected = False
if "FBUILD_DEV_MODE" not in os.environ:
    try:
        cwd = Path.cwd()
        pyproject_path = cwd / "pyproject.toml"
        if pyproject_path.exists():
            # Try tomllib (Python 3.11+) or tomli (fallback)
            try:
                import tomllib
            except ImportError:
                try:
                    import tomli as tomllib
                except ImportError:
                    tomllib = None

            if tomllib is not None:
                with open(pyproject_path, "rb") as f:
                    pyproject = tomllib.load(f)
                    package_name = pyproject.get("project", {}).get("name", "")
                    if package_name == "fbuild":
                        os.environ["FBUILD_DEV_MODE"] = "1"
                        _dev_mode_auto_detected = True
    except KeyboardInterrupt:
        raise
    except Exception:
        pass

# ruff: noqa: E402
# The following imports come after dev mode detection because paths.py reads FBUILD_DEV_MODE at import time
from fbuild import __version__
from fbuild.build.build_profiles import BuildProfile
from fbuild.cli_utils import (
    EnvironmentDetector,
    ErrorFormatter,
    MonitorFlagParser,
    PathValidator,
)
from fbuild.daemon import client as daemon_client
from fbuild.daemon.client.devices_http import (
    acquire_device_lease_http,
    get_device_status_http,
    list_devices_http,
    preempt_device_http,
    release_device_lease_http,
)
from fbuild.daemon.client.locks_http import (
    clear_stale_locks_http,
    display_lock_status_http,
)
from fbuild.daemon.client.requests_http import (
    request_build_http,
    request_deploy_http,
    request_monitor_http,
)
from fbuild.output import init_timer, log, log_header, set_verbose


@dataclass
class BuildArgs:
    """Arguments for the build command."""

    project_dir: Path
    environment: Optional[str] = None
    clean: bool = False
    verbose: bool = False
    jobs: Optional[int] = None
    profile: BuildProfile = BuildProfile.RELEASE
    platformio: bool = False
    explicit_release: bool = False
    explicit_quick: bool = False


@dataclass
class DeployArgs:
    """Arguments for the deploy command."""

    project_dir: Path
    environment: Optional[str] = None
    port: Optional[str] = None
    clean: bool = False
    monitor: Optional[str] = None
    verbose: bool = False
    qemu: bool = False
    qemu_timeout: int = 30
    platformio: bool = False


@dataclass
class MonitorArgs:
    """Arguments for the monitor command."""

    project_dir: Path
    environment: Optional[str] = None
    port: Optional[str] = None
    baud: int = 115200
    timeout: Optional[int] = None
    halt_on_error: Optional[str] = None
    halt_on_success: Optional[str] = None
    expect: Optional[str] = None
    verbose: bool = False
    timestamp: bool = True
    platformio: bool = False


def build_command(args: BuildArgs) -> None:
    """Build firmware for embedded target.

    Examples:
        fbuild build                      # Build default environment
        fbuild build tests/uno           # Build specific project
        fbuild build -e uno              # Build 'uno' environment
        fbuild build --clean             # Clean build
        fbuild build --verbose           # Verbose output
    """
    # Initialize timer and verbose mode
    init_timer()
    set_verbose(args.verbose)

    # Print header
    log_header("fbuild Build System", __version__)

    try:
        # Determine environment name
        env_name = EnvironmentDetector.detect_environment(args.project_dir, args.environment)

        # PlatformIO mode: bypass daemon, run pio directly
        if args.platformio:
            from fbuild.pio_runner import pio_build

            success = pio_build(
                project_dir=args.project_dir,
                environment=env_name,
                clean=args.clean,
                verbose=args.verbose,
                jobs=args.jobs,
                has_release=args.explicit_release,
                has_quick=args.explicit_quick,
            )
            sys.exit(0 if success else 1)

        # Show build start message
        if args.verbose:
            log(f"Building project: {args.project_dir}")
            log(f"Environment: {env_name}")
            log("")
        else:
            log(f"Building environment: {env_name}...")

        # Route build through daemon for background processing (HTTP-based)
        success = request_build_http(
            project_dir=args.project_dir,
            environment=env_name,
            clean_build=args.clean,
            verbose=args.verbose,
            jobs=args.jobs,
            profile=args.profile,
        )

        # Exit with appropriate code
        sys.exit(0 if success else 1)

    except RuntimeError as e:
        # Daemon startup failure
        ErrorFormatter.handle_unexpected_error(e, args.verbose)
        sys.exit(1)
    except FileNotFoundError as e:
        ErrorFormatter.handle_file_not_found(e)
    except PermissionError as e:
        ErrorFormatter.handle_permission_error(e)
    except KeyboardInterrupt as ke:
        from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

        handle_keyboard_interrupt_properly(ke)
    except Exception as e:
        ErrorFormatter.handle_unexpected_error(e, args.verbose)


def deploy_command(args: DeployArgs) -> None:
    """Deploy firmware to embedded target.

    Examples:
        fbuild deploy                     # Deploy default environment
        fbuild deploy tests/esp32c6      # Deploy specific project
        fbuild deploy -e esp32c6         # Deploy 'esp32c6' environment
        fbuild deploy -p COM3            # Deploy to specific port
        fbuild deploy --clean            # Clean build before deploy
        fbuild deploy --monitor="--timeout 60 --halt-on-success \"TEST PASSED\""  # Deploy and monitor
        fbuild deploy --qemu             # Deploy to QEMU emulator (requires Docker)
        fbuild deploy --qemu --qemu-timeout 60  # Deploy to QEMU with 60s timeout
    """
    # Initialize timer and verbose mode
    init_timer()
    set_verbose(args.verbose)

    log_header("fbuild Deployment System", __version__)

    try:
        # Determine environment name
        env_name = EnvironmentDetector.detect_environment(args.project_dir, args.environment)

        # PlatformIO mode: bypass daemon, run pio directly (QEMU takes precedence)
        if args.platformio and not args.qemu:
            from fbuild.pio_runner import pio_deploy

            success = pio_deploy(
                project_dir=args.project_dir,
                environment=env_name,
                port=args.port,
                clean=args.clean,
                verbose=args.verbose,
                monitor_flags=args.monitor,
            )
            sys.exit(0 if success else 1)

        # Handle QEMU deployment
        if args.qemu:
            log("Deploying to QEMU emulator...")
            from fbuild.config import PlatformIOConfig
            from fbuild.deploy.qemu_runner import (
                QEMURunner,
                ensure_docker_available,
                map_board_to_machine,
            )

            # Ensure Docker is available (attempts auto-start if not running)
            if not ensure_docker_available():
                ErrorFormatter.print_error("Docker is not available", "QEMU deployment requires Docker to be installed and running")
                print("\nInstall Docker:")
                print("  - Windows/Mac: https://www.docker.com/products/docker-desktop")
                print("  - Linux: https://docs.docker.com/engine/install/")
                sys.exit(1)

            # Load config to get board type
            ini_path = args.project_dir / "platformio.ini"
            if not ini_path.exists():
                ErrorFormatter.print_error("platformio.ini not found", str(ini_path))
                sys.exit(1)

            config = PlatformIOConfig(ini_path)
            env_config = config.get_env_config(env_name)
            board_id = env_config.get("board", "esp32dev")

            # Map board to QEMU machine type
            machine = map_board_to_machine(board_id)

            # Find firmware
            build_dir = args.project_dir / ".fbuild" / "build" / env_name
            firmware_bin = build_dir / "firmware.bin"

            if not firmware_bin.exists():
                ErrorFormatter.print_error("Firmware not found", f"Run 'fbuild build' first. Expected: {firmware_bin}")
                sys.exit(1)

            # Run QEMU
            runner = QEMURunner(verbose=args.verbose)
            exit_code = runner.run(
                firmware_path=firmware_bin,
                machine=machine,
                timeout=args.qemu_timeout,
            )

            sys.exit(exit_code)

        # Parse monitor flags if provided
        monitor_after = args.monitor is not None
        monitor_timeout = None
        monitor_halt_on_error = None
        monitor_halt_on_success = None
        monitor_expect = None
        monitor_show_timestamp = False
        if monitor_after and args.monitor is not None:
            flags = MonitorFlagParser.parse_monitor_flags(args.monitor)
            monitor_timeout = flags.timeout
            monitor_halt_on_error = flags.halt_on_error
            monitor_halt_on_success = flags.halt_on_success
            monitor_expect = flags.expect
            monitor_show_timestamp = flags.timestamp

        # Use daemon for concurrent deploy management (HTTP-based)
        success = request_deploy_http(
            project_dir=args.project_dir,
            environment=env_name,
            port=args.port,
            clean_build=args.clean,
            monitor_after=monitor_after,
            monitor_timeout=monitor_timeout,
            monitor_halt_on_error=monitor_halt_on_error,
            monitor_halt_on_success=monitor_halt_on_success,
            monitor_expect=monitor_expect,
            monitor_show_timestamp=monitor_show_timestamp,
            timeout=1800,  # 30 minute timeout for deploy
        )

        if success:
            sys.exit(0)
        else:
            sys.exit(1)

    except FileNotFoundError as e:
        ErrorFormatter.handle_file_not_found(e)
    except KeyboardInterrupt as ke:
        from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

        handle_keyboard_interrupt_properly(ke)
    except Exception as e:
        ErrorFormatter.handle_unexpected_error(e, args.verbose)


def monitor_command(args: MonitorArgs) -> None:
    """Monitor serial output from embedded target.

    Examples:
        fbuild monitor                                    # Monitor default environment
        fbuild monitor -p COM3                           # Monitor specific port
        fbuild monitor --timeout 60                      # Monitor with 60s timeout
        fbuild monitor --halt-on-error "ERROR"          # Exit on error
        fbuild monitor --halt-on-success "TEST PASSED"  # Exit on success
    """
    try:
        # Determine environment name
        env_name = EnvironmentDetector.detect_environment(args.project_dir, args.environment)

        # PlatformIO mode: bypass daemon, run pio directly
        if args.platformio:
            has_wrapping = args.timeout is not None or args.halt_on_error is not None or args.halt_on_success is not None or args.expect is not None
            if has_wrapping:
                from fbuild.pio_runner import pio_monitor_wrapped

                success = pio_monitor_wrapped(
                    project_dir=args.project_dir,
                    environment=env_name,
                    port=args.port,
                    baud=args.baud,
                    verbose=args.verbose,
                    timeout=args.timeout,
                    halt_on_error=args.halt_on_error,
                    halt_on_success=args.halt_on_success,
                    expect=args.expect,
                )
            else:
                from fbuild.pio_runner import pio_monitor

                success = pio_monitor(
                    project_dir=args.project_dir,
                    environment=env_name,
                    port=args.port,
                    baud=args.baud,
                    verbose=args.verbose,
                )
            sys.exit(0 if success else 1)

        # Use daemon for concurrent monitor management (HTTP-based)
        success = request_monitor_http(
            project_dir=args.project_dir,
            environment=env_name,
            port=args.port,
            baud_rate=args.baud,
            halt_on_error=args.halt_on_error,
            halt_on_success=args.halt_on_success,
            expect=args.expect,
            timeout=args.timeout,
            show_timestamp=args.timestamp,
        )

        if success:
            sys.exit(0)
        else:
            sys.exit(1)

    except FileNotFoundError as e:
        ErrorFormatter.handle_file_not_found(e)
    except KeyboardInterrupt as ke:
        from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

        handle_keyboard_interrupt_properly(ke)
    except Exception as e:
        ErrorFormatter.handle_unexpected_error(e, args.verbose)


def device_command(
    action: str,
    device_id: Optional[str] = None,
    lease_type: str = "exclusive",
    description: str = "",
    reason: str = "",
    refresh: bool = False,
) -> None:
    """Manage devices connected to the daemon.

    Examples:
        fbuild device list                           # List all connected devices
        fbuild device list --refresh                 # Refresh device discovery before listing
        fbuild device status <device_id>             # Show detailed device status
        fbuild device lease <device_id>              # Acquire exclusive lease on device
        fbuild device lease <device_id> --monitor    # Acquire monitor (read-only) lease
        fbuild device release <device_id>            # Release lease on device
        fbuild device take <device_id> --reason "Urgent deployment"  # Preempt current holder
    """
    try:
        if action == "list":
            # List all devices
            devices = list_devices_http(refresh=refresh)
            if devices is None:
                ErrorFormatter.print_error("Failed to list devices", "Daemon may not be running")
                sys.exit(1)

            if not devices:
                print("No devices found")
                sys.exit(0)

            print(f"Found {len(devices)} device(s):\n")
            for device in devices:
                device_id = device.get("device_id", "unknown")
                port = device.get("port", "unknown")
                connected = "✅ connected" if device.get("is_connected", False) else "❌ disconnected"
                exclusive = device.get("exclusive_holder")
                monitor_count = device.get("monitor_count", 0)

                print(f"  {device_id}")
                print(f"    Port: {port}")
                print(f"    Status: {connected}")
                if exclusive:
                    print(f"    Exclusive holder: {exclusive}")
                if monitor_count > 0:
                    print(f"    Monitor sessions: {monitor_count}")
                print()

            sys.exit(0)

        elif action == "status":
            if not device_id:
                ErrorFormatter.print_error("Device ID required", "Usage: fbuild device status <device_id>")
                sys.exit(1)

            status = get_device_status_http(device_id)
            if status is None:
                ErrorFormatter.print_error(f"Device not found: {device_id}", "")
                sys.exit(1)

            print(f"Device: {device_id}")
            print(f"  Connected: {'✅ Yes' if status.get('is_connected') else '❌ No'}")
            print(f"  Port: {status.get('device_info', {}).get('port', 'unknown')}")
            print(f"  Available for exclusive: {'✅ Yes' if status.get('is_available_for_exclusive') else '❌ No'}")

            if status.get("exclusive_lease"):
                lease = status["exclusive_lease"]
                print(f"  Exclusive holder: {lease.get('client_id', 'unknown')}")
                print(f"    Description: {lease.get('description', 'N/A')}")

            if status.get("monitor_count", 0) > 0:
                print(f"  Monitor sessions: {status['monitor_count']}")
                for monitor in status.get("monitor_leases", []):
                    print(f"    - {monitor.get('client_id', 'unknown')}")

            sys.exit(0)

        elif action == "lease":
            if not device_id:
                ErrorFormatter.print_error("Device ID required", "Usage: fbuild device lease <device_id>")
                sys.exit(1)

            result = acquire_device_lease_http(
                device_id=device_id,
                lease_type=lease_type,
                description=description,
            )

            if result is None:
                ErrorFormatter.print_error("Failed to acquire lease", "Daemon may not be running")
                sys.exit(1)

            if result.get("success"):
                lease_id = result.get("lease_id", "unknown")
                print(f"✅ Acquired {lease_type} lease on device {device_id}")
                print(f"   Lease ID: {lease_id}")
                sys.exit(0)
            else:
                ErrorFormatter.print_error(f"Failed to acquire lease: {result.get('message', 'unknown error')}", "")
                sys.exit(1)

        elif action == "release":
            if not device_id:
                ErrorFormatter.print_error("Device ID or lease ID required", "Usage: fbuild device release <device_id>")
                sys.exit(1)

            result = release_device_lease_http(device_id)

            if result is None:
                ErrorFormatter.print_error("Failed to release lease", "Daemon may not be running")
                sys.exit(1)

            if result.get("success"):
                print(f"✅ Released lease on device {device_id}")
                sys.exit(0)
            else:
                ErrorFormatter.print_error(f"Failed to release lease: {result.get('message', 'unknown error')}", "")
                sys.exit(1)

        elif action == "take":
            if not device_id:
                ErrorFormatter.print_error("Device ID required", 'Usage: fbuild device take <device_id> --reason "..."')
                sys.exit(1)

            if not reason:
                ErrorFormatter.print_error("Reason required for preemption", 'Usage: fbuild device take <device_id> --reason "..."')
                sys.exit(1)

            result = preempt_device_http(device_id, reason)

            if result is None:
                ErrorFormatter.print_error("Failed to preempt device", "Daemon may not be running")
                sys.exit(1)

            if result.get("success"):
                preempted = result.get("preempted_client_id")
                print(f"✅ Preempted device {device_id}")
                if preempted:
                    print(f"   Previous holder: {preempted}")
                print(f"   Lease ID: {result.get('lease_id', 'unknown')}")
                sys.exit(0)
            else:
                ErrorFormatter.print_error(f"Failed to preempt device: {result.get('message', 'unknown error')}", "")
                sys.exit(1)

        else:
            ErrorFormatter.print_error(f"Unknown device action: {action}", "")
            print("Valid actions: list, status, lease, release, take")
            sys.exit(1)

    except KeyboardInterrupt as ke:
        from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

        handle_keyboard_interrupt_properly(ke)
    except Exception as e:
        ErrorFormatter.handle_unexpected_error(e, verbose=False)


def show_command(target: str, follow: bool = True, lines: int = 50) -> None:
    """Show daemon logs or other information.

    Examples:
        fbuild show daemon             # Tail daemon logs (Ctrl-C to stop, daemon continues)
        fbuild show daemon --no-follow # Show last 50 lines and exit
        fbuild show daemon --lines 100 # Show last 100 lines then follow
    """
    try:
        if target == "daemon":
            daemon_client.tail_daemon_logs(follow=follow, lines=lines)
            sys.exit(0)
        else:
            from fbuild.cli_utils import ErrorFormatter

            ErrorFormatter.print_error(f"Unknown target: {target}", "")
            print("Valid targets: daemon")
            sys.exit(1)

    except KeyboardInterrupt as ke:
        from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

        handle_keyboard_interrupt_properly(ke)
    except Exception as e:
        from fbuild.cli_utils import ErrorFormatter

        ErrorFormatter.handle_unexpected_error(e, verbose=False)


def daemon_command(action: str, pid: Optional[int] = None, force: bool = False, follow: bool = True, lines: int = 50) -> None:
    """Manage the fbuild daemon.

    Examples:
        fbuild daemon status       # Show daemon status
        fbuild daemon stop         # Stop the daemon
        fbuild daemon restart      # Restart the daemon
        fbuild daemon list         # List all daemon instances
        fbuild daemon locks        # Show lock status
        fbuild daemon clear-locks  # Clear stale locks
        fbuild daemon monitor      # Tail daemon logs (alias for 'fbuild show daemon')
        fbuild daemon kill --pid 12345        # Kill specific daemon
        fbuild daemon kill-all               # Kill all daemons
        fbuild daemon kill-all --force       # Force kill all daemons
    """
    try:
        if action == "status":
            # Get daemon status
            status = daemon_client.get_daemon_status()

            if status["running"]:
                print("✅ Daemon is running")
                print(f"   PID: {status.get('pid', 'unknown')}")

                if "current_status" in status:
                    current = status["current_status"]
                    print(f"   State: {current.get('state', 'unknown')}")
                    print(f"   Message: {current.get('message', 'N/A')}")

                    if current.get("operation_in_progress"):
                        print("   🔄 Operation in progress:")
                        print(f"      Environment: {current.get('environment', 'N/A')}")
                        print(f"      Project: {current.get('project_dir', 'N/A')}")
            else:
                print("❌ Daemon is not running")

        elif action == "stop":
            # Stop daemon
            if daemon_client.stop_daemon():
                sys.exit(0)
            else:
                ErrorFormatter.print_error("Failed to stop daemon", "")
                sys.exit(1)

        elif action == "restart":
            # Restart daemon
            print("Restarting daemon...")
            if daemon_client.is_daemon_running():
                if not daemon_client.stop_daemon():
                    ErrorFormatter.print_error("Failed to stop daemon", "")
                    sys.exit(1)

            # Start fresh daemon
            if daemon_client.ensure_daemon_running():
                print("✅ Daemon restarted successfully")
                sys.exit(0)
            else:
                ErrorFormatter.print_error("Failed to restart daemon", "")
                sys.exit(1)

        elif action == "list":
            # List all daemon instances
            daemon_client.display_daemon_list()

        elif action == "locks":
            # Show lock status
            display_lock_status_http()

        elif action == "clear-locks":
            # Clear stale locks
            result = clear_stale_locks_http()
            if result.get("success"):
                print(f"✅ {result.get('message', 'Locks cleared')}")
                sys.exit(0)
            else:
                ErrorFormatter.print_error(f"Failed to clear locks: {result.get('message', 'Unknown error')}", "")
                sys.exit(1)

        elif action == "kill":
            # Kill specific daemon by PID
            if pid is None:
                ErrorFormatter.print_error("--pid required for kill action", "")
                print("Usage: fbuild daemon kill --pid <PID> [--force]")
                sys.exit(1)

            if force:
                success = daemon_client.force_kill_daemon(pid)
            else:
                success = daemon_client.graceful_kill_daemon(pid)

            if success:
                print(f"✅ Daemon (PID {pid}) terminated")
                sys.exit(0)
            else:
                ErrorFormatter.print_error(f"Failed to terminate daemon (PID {pid})", "Process may not exist")
                sys.exit(1)

        elif action == "kill-all":
            # Kill all daemon instances
            killed = daemon_client.kill_all_daemons(force=force)
            if killed > 0:
                print(f"✅ Killed {killed} daemon instance(s)")
            else:
                print("No daemon instances found to kill")
            sys.exit(0)

        elif action == "monitor":
            # Monitor daemon logs (tail the log file)
            daemon_client.tail_daemon_logs(follow=follow, lines=lines)
            sys.exit(0)

        else:
            ErrorFormatter.print_error(f"Unknown daemon action: {action}", "")
            print("Valid actions: status, stop, restart, list, locks, clear-locks, monitor, kill, kill-all")
            sys.exit(1)

    except KeyboardInterrupt as ke:
        from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

        handle_keyboard_interrupt_properly(ke)
    except Exception as e:
        ErrorFormatter.handle_unexpected_error(e, verbose=False)


def parse_default_action_args(argv: list[str]) -> DeployArgs:
    """Parse arguments for the default action (fbuild <project_dir> [flags]).

    Args:
        argv: Command-line arguments (sys.argv)

    Returns:
        DeployArgs with parsed values

    Raises:
        SystemExit: If project directory is invalid or required arguments are missing
    """
    if len(argv) < 2:
        ErrorFormatter.print_error("Missing project directory", "")
        sys.exit(1)

    project_dir = Path(argv[1])
    PathValidator.validate_project_dir(project_dir)

    # Parse remaining arguments
    monitor: Optional[str] = None
    port: Optional[str] = None
    environment: Optional[str] = None
    clean = False
    verbose = False
    platformio = False

    i = 2
    while i < len(argv):
        arg = argv[i]

        # Handle --monitor flag
        if arg.startswith("--monitor="):
            monitor = arg.split("=", 1)[1]
            i += 1
        elif arg == "--monitor" and i + 1 < len(argv):
            monitor = argv[i + 1]
            i += 2
        # Handle --port flag
        elif arg.startswith("--port="):
            port = arg.split("=", 1)[1]
            i += 1
        elif arg in ("-p", "--port") and i + 1 < len(argv):
            port = argv[i + 1]
            i += 2
        # Handle --environment flag
        elif arg.startswith("--environment="):
            environment = arg.split("=", 1)[1]
            i += 1
        elif arg.startswith("-e="):
            environment = arg.split("=", 1)[1]
            i += 1
        elif arg in ("-e", "--environment") and i + 1 < len(argv):
            environment = argv[i + 1]
            i += 2
        # Handle --clean flag
        elif arg in ("-c", "--clean"):
            clean = True
            i += 1
        # Handle --verbose flag
        elif arg in ("-v", "--verbose"):
            verbose = True
            i += 1
        # Handle --platformio flag
        elif arg == "--platformio":
            platformio = True
            i += 1
        else:
            # Unknown flag - warn and skip
            ErrorFormatter.print_error(f"Unknown flag in default action: {arg}", "")
            print("Hint: Use 'fbuild deploy --help' to see available flags")
            sys.exit(1)

    return DeployArgs(
        project_dir=project_dir,
        environment=environment,
        port=port,
        clean=clean,
        monitor=monitor if monitor is not None else "",  # Empty string means monitor with default settings
        verbose=verbose,
        platformio=platformio,
    )


def main() -> None:
    """fbuild - Modern embedded build system.

    Replace PlatformIO with URL-based platform/toolchain management.
    """
    # Print dev mode notification if it was auto-detected (set before imports)
    if _dev_mode_auto_detected:
        print("🔧 FBUILD_DEV_MODE=1 (auto-detected from pyproject.toml)")
        print()

    # Configure UTF-8 encoding for stdout/stderr to support emojis on Windows
    # This prevents UnicodeEncodeError when printing emojis on Windows (cp1252 encoding)
    # Skip reconfiguration in test environments (pytest capture, redirected streams)
    if sys.platform == "win32":
        import io

        try:
            # Check if we're in a test environment (pytest capture or redirected streams)
            # pytest's capture wraps stdout/stderr, so we detect this by checking the class name
            is_pytest_capture = any("pytest" in type(stream).__module__.lower() or "capture" in type(stream).__name__.lower() for stream in [sys.stdout, sys.stderr] if hasattr(stream, "__module__"))

            # Only reconfigure if not in test environment and buffer exists
            if not is_pytest_capture and hasattr(sys.stdout, "buffer") and hasattr(sys.stderr, "buffer"):
                # Don't reassign if already UTF-8 to avoid closing existing streams
                if sys.stdout.encoding.lower() != "utf-8":
                    sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8", errors="replace", line_buffering=True)
                if sys.stderr.encoding.lower() != "utf-8":
                    sys.stderr = io.TextIOWrapper(sys.stderr.buffer, encoding="utf-8", errors="replace", line_buffering=True)
        except (ValueError, AttributeError):
            # Ignore errors in test environment or when buffer is closed
            pass

    # Display daemon stats as the first action (unless --version or --help anywhere)
    help_flags = {"--version", "-V", "--help", "-h"}
    skip_stats = any(arg in help_flags for arg in sys.argv)
    if len(sys.argv) >= 2 and not skip_stats:
        try:
            daemon_client.display_daemon_stats_compact()
            print()  # Blank line after stats
        except (ValueError, OSError):
            # Ignore if stdout is closed (e.g., in test environment)
            pass

    # Handle default action: fbuild <project_dir> [flags] → deploy with monitor
    # This check must happen before argparse to avoid conflicts
    if len(sys.argv) >= 2 and not sys.argv[1].startswith("-") and sys.argv[1] not in ["build", "deploy", "monitor", "daemon", "device", "show"]:
        # User provided a path without a subcommand - use default action
        deploy_args = parse_default_action_args(sys.argv)
        deploy_command(deploy_args)
        return

    parser = argparse.ArgumentParser(
        prog="fbuild",
        description="fbuild - Modern embedded build system",
    )
    parser.add_argument(
        "--version",
        action="version",
        version=f"fbuild {__version__}",
    )

    subparsers = parser.add_subparsers(dest="command", help="Command to run")

    # Build command
    build_parser = subparsers.add_parser(
        "build",
        help="Build firmware for embedded target",
    )
    build_parser.add_argument(
        "project_dir",
        nargs="?",
        type=Path,
        default=Path.cwd(),
        help="Project directory (default: current directory)",
    )
    build_parser.add_argument(
        "-e",
        "--environment",
        default=None,
        help="Build environment (default: auto-detect from platformio.ini)",
    )
    build_parser.add_argument(
        "-c",
        "--clean",
        action="store_true",
        help="Clean build artifacts before building",
    )
    build_parser.add_argument(
        "-v",
        "--verbose",
        action="store_true",
        help="Show verbose build output",
    )
    build_parser.add_argument(
        "-j",
        "--jobs",
        type=int,
        default=os.cpu_count(),
        help="Number of parallel compilation jobs (default: CPU count, use 1 for serial compilation)",
    )
    # Build profile (mutually exclusive)
    profile_group = build_parser.add_mutually_exclusive_group()
    profile_group.add_argument(
        "--release",
        action="store_true",
        help="Optimized release build with LTO and -O3 (default)",
    )
    profile_group.add_argument(
        "--quick",
        action="store_true",
        help="Fast development build (no LTO, -O2, section GC)",
    )
    build_parser.add_argument(
        "--platformio",
        action="store_true",
        help="Delegate build to PlatformIO CLI instead of fbuild's native build system",
    )

    # Deploy command
    deploy_parser = subparsers.add_parser(
        "deploy",
        help="Deploy firmware to embedded target",
    )
    deploy_parser.add_argument(
        "project_dir",
        nargs="?",
        type=Path,
        default=Path.cwd(),
        help="Project directory (default: current directory)",
    )
    deploy_parser.add_argument(
        "-e",
        "--environment",
        default=None,
        help="Build environment (default: auto-detect from platformio.ini)",
    )
    deploy_parser.add_argument(
        "-p",
        "--port",
        default=None,
        help="Serial port (default: auto-detect)",
    )
    deploy_parser.add_argument(
        "-c",
        "--clean",
        action="store_true",
        help="Clean build artifacts before building",
    )
    deploy_parser.add_argument(
        "--monitor",
        default=None,
        help="Monitor flags to pass after deployment (e.g., '--timeout 60 --halt-on-success \"TEST PASSED\"')",
    )
    deploy_parser.add_argument(
        "--qemu",
        action="store_true",
        help="Deploy to QEMU emulator instead of physical device (requires Docker)",
    )
    deploy_parser.add_argument(
        "--qemu-timeout",
        type=int,
        default=30,
        help="Timeout in seconds for QEMU execution (default: 30)",
    )
    deploy_parser.add_argument(
        "-v",
        "--verbose",
        action="store_true",
        help="Show verbose output",
    )
    deploy_parser.add_argument(
        "--platformio",
        action="store_true",
        help="Delegate deploy to PlatformIO CLI instead of fbuild's native deploy system",
    )

    # Monitor command
    monitor_parser = subparsers.add_parser(
        "monitor",
        help="Monitor serial output from embedded target",
    )
    monitor_parser.add_argument(
        "project_dir",
        nargs="?",
        type=Path,
        default=Path.cwd(),
        help="Project directory (default: current directory)",
    )
    monitor_parser.add_argument(
        "-e",
        "--environment",
        default=None,
        help="Build environment (default: auto-detect from platformio.ini)",
    )
    monitor_parser.add_argument(
        "-p",
        "--port",
        default=None,
        help="Serial port (default: auto-detect)",
    )
    monitor_parser.add_argument(
        "-b",
        "--baud",
        default=115200,
        type=int,
        help="Baud rate (default: 115200)",
    )
    monitor_parser.add_argument(
        "-t",
        "--timeout",
        default=None,
        type=int,
        help="Timeout in seconds (default: no timeout)",
    )
    monitor_parser.add_argument(
        "--halt-on-error",
        default=None,
        help="Pattern that triggers error exit (regex)",
    )
    monitor_parser.add_argument(
        "--halt-on-success",
        default=None,
        help="Pattern that triggers success exit (regex)",
    )
    monitor_parser.add_argument(
        "--expect",
        default=None,
        help="Expected pattern - checked at timeout/success, exit 0 if found, 1 if not (regex)",
    )
    monitor_parser.add_argument(
        "--no-timestamp",
        action="store_true",
        dest="no_timestamp",
        help="Disable timestamp prefix on each output line (timestamps enabled by default)",
    )
    monitor_parser.add_argument(
        "-v",
        "--verbose",
        action="store_true",
        help="Show verbose output",
    )
    monitor_parser.add_argument(
        "--platformio",
        action="store_true",
        help="Delegate monitor to PlatformIO CLI instead of fbuild's native monitor",
    )

    # Show command
    show_parser = subparsers.add_parser(
        "show",
        help="Show daemon logs or other information",
    )
    show_parser.add_argument(
        "target",
        choices=["daemon"],
        help="What to show (currently only 'daemon' for daemon logs)",
    )
    show_parser.add_argument(
        "--no-follow",
        action="store_true",
        dest="no_follow",
        help="Don't follow the log file (just print last lines and exit)",
    )
    show_parser.add_argument(
        "--lines",
        type=int,
        default=50,
        help="Number of lines to show initially (default: 50)",
    )

    # Daemon command
    daemon_parser = subparsers.add_parser(
        "daemon",
        help="Manage the fbuild daemon",
    )
    daemon_parser.add_argument(
        "action",
        choices=["status", "stop", "restart", "list", "locks", "clear-locks", "monitor", "kill", "kill-all"],
        help="Daemon action to perform",
    )
    daemon_parser.add_argument(
        "--pid",
        type=int,
        default=None,
        help="PID of daemon to kill (required for 'kill' action)",
    )
    daemon_parser.add_argument(
        "--force",
        action="store_true",
        help="Force kill without graceful shutdown (for 'kill' and 'kill-all' actions)",
    )
    daemon_parser.add_argument(
        "--no-follow",
        action="store_true",
        dest="no_follow",
        help="Don't follow the log file, just print last lines and exit (for 'monitor' action)",
    )
    daemon_parser.add_argument(
        "--lines",
        type=int,
        default=50,
        help="Number of lines to show initially (for 'monitor' action, default: 50)",
    )

    # Device command
    device_parser = subparsers.add_parser(
        "device",
        help="Manage devices connected to the daemon",
    )
    device_parser.add_argument(
        "action",
        choices=["list", "status", "lease", "release", "take"],
        help="Device action to perform",
    )
    device_parser.add_argument(
        "device_id",
        nargs="?",
        default=None,
        help="Device ID (required for status, lease, release, take)",
    )
    device_parser.add_argument(
        "--monitor",
        action="store_true",
        dest="lease_monitor",
        help="Acquire monitor (read-only) lease instead of exclusive (for 'lease' action)",
    )
    device_parser.add_argument(
        "--description",
        default="",
        help="Description for lease (for 'lease' action)",
    )
    device_parser.add_argument(
        "--reason",
        default="",
        help="Reason for preemption (required for 'take' action)",
    )
    device_parser.add_argument(
        "--refresh",
        action="store_true",
        help="Refresh device discovery before listing (for 'list' action)",
    )

    # Parse arguments
    parsed_args = parser.parse_args()

    # If no command specified, show help
    if not parsed_args.command:
        parser.print_help()
        sys.exit(0)

    # Validate project directory exists
    if hasattr(parsed_args, "project_dir"):
        PathValidator.validate_project_dir(parsed_args.project_dir)

    # Validate jobs parameter
    if hasattr(parsed_args, "jobs") and parsed_args.jobs is not None:
        if parsed_args.jobs < 1:
            log(f"❌ Error: --jobs must be at least 1 (got {parsed_args.jobs})")
            sys.exit(1)

    # Execute command
    if parsed_args.command == "build":
        # Determine profile: --quick sets quick, otherwise release (default)
        profile = BuildProfile.QUICK if parsed_args.quick else BuildProfile.RELEASE
        build_args = BuildArgs(
            project_dir=parsed_args.project_dir,
            environment=parsed_args.environment,
            clean=parsed_args.clean,
            verbose=parsed_args.verbose,
            jobs=parsed_args.jobs,
            profile=profile,
            platformio=parsed_args.platformio,
            explicit_release=parsed_args.release,
            explicit_quick=parsed_args.quick,
        )
        build_command(build_args)
    elif parsed_args.command == "deploy":
        deploy_args = DeployArgs(
            project_dir=parsed_args.project_dir,
            environment=parsed_args.environment,
            port=parsed_args.port,
            clean=parsed_args.clean,
            monitor=parsed_args.monitor,
            verbose=parsed_args.verbose,
            qemu=parsed_args.qemu,
            qemu_timeout=parsed_args.qemu_timeout,
            platformio=parsed_args.platformio,
        )
        deploy_command(deploy_args)
    elif parsed_args.command == "monitor":
        monitor_args = MonitorArgs(
            project_dir=parsed_args.project_dir,
            environment=parsed_args.environment,
            port=parsed_args.port,
            baud=parsed_args.baud,
            timeout=parsed_args.timeout,
            halt_on_error=parsed_args.halt_on_error,
            halt_on_success=parsed_args.halt_on_success,
            expect=parsed_args.expect,
            verbose=parsed_args.verbose,
            timestamp=not parsed_args.no_timestamp,
            platformio=parsed_args.platformio,
        )
        monitor_command(monitor_args)
    elif parsed_args.command == "daemon":
        daemon_command(
            parsed_args.action,
            pid=parsed_args.pid,
            force=parsed_args.force,
            follow=not parsed_args.no_follow,
            lines=parsed_args.lines,
        )
    elif parsed_args.command == "show":
        show_command(
            target=parsed_args.target,
            follow=not parsed_args.no_follow,
            lines=parsed_args.lines,
        )
    elif parsed_args.command == "device":
        lease_type = "monitor" if parsed_args.lease_monitor else "exclusive"
        device_command(
            action=parsed_args.action,
            device_id=parsed_args.device_id,
            lease_type=lease_type,
            description=parsed_args.description,
            reason=parsed_args.reason,
            refresh=parsed_args.refresh,
        )


if __name__ == "__main__":
    main()
