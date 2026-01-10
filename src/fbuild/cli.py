"""
Command-line interface for fbuild.

This module provides the `fbuild` CLI tool for building embedded firmware.
"""

import argparse
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

from fbuild.cli_utils import (
    EnvironmentDetector,
    ErrorFormatter,
    MonitorFlagParser,
    PathValidator,
)
from fbuild.daemon import client as daemon_client


@dataclass
class BuildArgs:
    """Arguments for the build command."""

    project_dir: Path
    environment: Optional[str] = None
    clean: bool = False
    verbose: bool = False


@dataclass
class DeployArgs:
    """Arguments for the deploy command."""

    project_dir: Path
    environment: Optional[str] = None
    port: Optional[str] = None
    clean: bool = False
    monitor: Optional[str] = None
    verbose: bool = False


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


def build_command(args: BuildArgs) -> None:
    """Build firmware for embedded target.

    Examples:
        fbuild build                      # Build default environment
        fbuild build tests/uno           # Build specific project
        fbuild build -e uno              # Build 'uno' environment
        fbuild build --clean             # Clean build
        fbuild build --verbose           # Verbose output
    """
    # Print header
    print("fbuild Build System v0.1.0")
    print()

    try:
        # Determine environment name
        env_name = EnvironmentDetector.detect_environment(args.project_dir, args.environment)

        # Show build start message
        if args.verbose:
            print(f"Building project: {args.project_dir}")
            print(f"Environment: {env_name}")
            print()
        else:
            print(f"Building environment: {env_name}...")

        # Route build through daemon for background processing
        success = daemon_client.request_build(
            project_dir=args.project_dir,
            environment=env_name,
            clean_build=args.clean,
            verbose=args.verbose,
        )

        # Exit with appropriate code
        sys.exit(0 if success else 1)

    except FileNotFoundError as e:
        ErrorFormatter.handle_file_not_found(e)
    except PermissionError as e:
        ErrorFormatter.handle_permission_error(e)
    except KeyboardInterrupt:
        ErrorFormatter.handle_keyboard_interrupt()
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
    """
    print("fbuild Deployment System v0.1.0")
    print()

    try:
        # Determine environment name
        env_name = EnvironmentDetector.detect_environment(args.project_dir, args.environment)

        # Parse monitor flags if provided
        monitor_after = args.monitor is not None
        monitor_timeout = None
        monitor_halt_on_error = None
        monitor_halt_on_success = None
        monitor_expect = None
        if monitor_after and args.monitor is not None:
            flags = MonitorFlagParser.parse_monitor_flags(args.monitor)
            monitor_timeout = flags.timeout
            monitor_halt_on_error = flags.halt_on_error
            monitor_halt_on_success = flags.halt_on_success
            monitor_expect = flags.expect

        # Use daemon for concurrent deploy management
        success = daemon_client.request_deploy(
            project_dir=args.project_dir,
            environment=env_name,
            port=args.port,
            clean_build=args.clean,
            monitor_after=monitor_after,
            monitor_timeout=monitor_timeout,
            monitor_halt_on_error=monitor_halt_on_error,
            monitor_halt_on_success=monitor_halt_on_success,
            monitor_expect=monitor_expect,
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

        # Use daemon for concurrent monitor management
        success = daemon_client.request_monitor(
            project_dir=args.project_dir,
            environment=env_name,
            port=args.port,
            baud_rate=args.baud,
            halt_on_error=args.halt_on_error,
            halt_on_success=args.halt_on_success,
            expect=args.expect,
            timeout=args.timeout,
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


def daemon_command(action: str) -> None:
    """Manage the fbuild daemon.

    Examples:
        fbuild daemon status    # Show daemon status
        fbuild daemon stop      # Stop the daemon
        fbuild daemon restart   # Restart the daemon
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
        else:
            ErrorFormatter.print_error(f"Unknown daemon action: {action}", "")
            print("Valid actions: status, stop, restart")
            sys.exit(1)

    except KeyboardInterrupt:
        ErrorFormatter.handle_keyboard_interrupt()
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
    )


def main() -> None:
    """fbuild - Modern embedded build system.

    Replace PlatformIO with URL-based platform/toolchain management.
    """
    # Handle default action: fbuild <project_dir> [flags] → deploy with monitor
    # This check must happen before argparse to avoid conflicts
    if len(sys.argv) >= 2 and not sys.argv[1].startswith("-") and sys.argv[1] not in ["build", "deploy", "monitor", "daemon"]:
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
        version="fbuild 0.1.0",
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
        "-v",
        "--verbose",
        action="store_true",
        help="Show verbose output",
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
        "-v",
        "--verbose",
        action="store_true",
        help="Show verbose output",
    )

    # Daemon command
    daemon_parser = subparsers.add_parser(
        "daemon",
        help="Manage the fbuild daemon",
    )
    daemon_parser.add_argument(
        "action",
        choices=["status", "stop", "restart"],
        help="Daemon action to perform",
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

    # Execute command
    if parsed_args.command == "build":
        build_args = BuildArgs(
            project_dir=parsed_args.project_dir,
            environment=parsed_args.environment,
            clean=parsed_args.clean,
            verbose=parsed_args.verbose,
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
        )
        monitor_command(monitor_args)
    elif parsed_args.command == "daemon":
        daemon_command(parsed_args.action)


if __name__ == "__main__":
    main()
