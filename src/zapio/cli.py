"""
Command-line interface for Zapio.

This module provides the `zap` CLI tool for building embedded firmware.
"""

import argparse
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

from zapio.build import BuildOrchestratorAVR
from zapio.cli_utils import (
    EnvironmentDetector,
    ErrorFormatter,
    MonitorFlagParser,
    PathValidator,
)
from zapio.daemon import client as daemon_client


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
    verbose: bool = False


def build_command(args: BuildArgs) -> None:
    """Build firmware for embedded target.

    Examples:
        zap build                      # Build default environment
        zap build tests/uno           # Build specific project
        zap build -e uno              # Build 'uno' environment
        zap build --clean             # Clean build
        zap build --verbose           # Verbose output
    """
    # Print header
    print("Zapio Build System v0.1.0")
    print()

    try:
        # Create orchestrator
        orchestrator = BuildOrchestratorAVR(verbose=args.verbose)

        # Determine environment name
        env_name = EnvironmentDetector.detect_environment(
            args.project_dir, args.environment
        )

        # Show build start message
        if args.verbose:
            print(f"Building project: {args.project_dir}")
            print(f"Environment: {env_name}")
            print()
        else:
            print(f"Building environment: {env_name}...")

        # Perform build
        start_time = time.time()
        result = orchestrator.build(
            project_dir=args.project_dir,
            env_name=env_name,
            clean=args.clean,
            verbose=args.verbose,
        )
        build_time = time.time() - start_time

        # Check result
        if result.success:
            # Success output
            ErrorFormatter.print_success("Build successful!")
            print()
            print(f"Firmware: {result.hex_path}")

            # Display size information
            if result.size_info:
                size_info = result.size_info
                print()
                print("Firmware Size:")

                # Program memory (Flash)
                flash_bytes = size_info.total_flash
                if size_info.max_flash:
                    flash_percent = (flash_bytes / size_info.max_flash) * 100
                    print(
                        f"  Program:  {flash_bytes:>6} bytes ({flash_percent:>5.1f}% of {size_info.max_flash} bytes)"
                    )
                else:
                    print(f"  Program:  {flash_bytes:>6} bytes")

                # RAM usage
                ram_bytes = size_info.data + size_info.bss
                if size_info.max_ram:
                    ram_percent = (ram_bytes / size_info.max_ram) * 100
                    print(
                        f"  RAM:      {ram_bytes:>6} bytes ({ram_percent:>5.1f}% of {size_info.max_ram} bytes)"
                    )
                else:
                    print(f"  RAM:      {ram_bytes:>6} bytes")

                print()

            print(f"Build time: {build_time:.2f}s")
            sys.exit(0)
        else:
            # Failure output
            ErrorFormatter.print_error("Build failed!", result.message)
            sys.exit(1)

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
        zap deploy                     # Deploy default environment
        zap deploy tests/esp32c6      # Deploy specific project
        zap deploy -e esp32c6         # Deploy 'esp32c6' environment
        zap deploy -p COM3            # Deploy to specific port
        zap deploy --clean            # Clean build before deploy
        zap deploy --monitor="--timeout 60 --halt-on-success \"TEST PASSED\""  # Deploy and monitor
    """
    print("Zapio Deployment System v0.1.0")
    print()

    try:
        # Determine environment name
        env_name = EnvironmentDetector.detect_environment(
            args.project_dir, args.environment
        )

        # Parse monitor flags if provided
        monitor_after = args.monitor is not None
        monitor_timeout = None
        monitor_halt_on_error = None
        monitor_halt_on_success = None
        if monitor_after and args.monitor is not None:
            flags = MonitorFlagParser.parse_monitor_flags(args.monitor)
            monitor_timeout = flags.timeout
            monitor_halt_on_error = flags.halt_on_error
            monitor_halt_on_success = flags.halt_on_success

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
            timeout=1800,  # 30 minute timeout for deploy
        )

        if success:
            sys.exit(0)
        else:
            sys.exit(1)

    except FileNotFoundError as e:
        ErrorFormatter.handle_file_not_found(e)
    except KeyboardInterrupt as ke:
        from zapio.interrupt_utils import handle_keyboard_interrupt_properly

        handle_keyboard_interrupt_properly(ke)
    except Exception as e:
        ErrorFormatter.handle_unexpected_error(e, args.verbose)


def monitor_command(args: MonitorArgs) -> None:
    """Monitor serial output from embedded target.

    Examples:
        zap monitor                                    # Monitor default environment
        zap monitor -p COM3                           # Monitor specific port
        zap monitor --timeout 60                      # Monitor with 60s timeout
        zap monitor --halt-on-error "ERROR"          # Exit on error
        zap monitor --halt-on-success "TEST PASSED"  # Exit on success
    """
    try:
        # Determine environment name
        env_name = EnvironmentDetector.detect_environment(
            args.project_dir, args.environment
        )

        # Use daemon for concurrent monitor management
        success = daemon_client.request_monitor(
            project_dir=args.project_dir,
            environment=env_name,
            port=args.port,
            baud_rate=args.baud,
            halt_on_error=args.halt_on_error,
            halt_on_success=args.halt_on_success,
            timeout=args.timeout,
        )

        if success:
            sys.exit(0)
        else:
            sys.exit(1)

    except FileNotFoundError as e:
        ErrorFormatter.handle_file_not_found(e)
    except KeyboardInterrupt as ke:
        from zapio.interrupt_utils import handle_keyboard_interrupt_properly

        handle_keyboard_interrupt_properly(ke)
    except Exception as e:
        ErrorFormatter.handle_unexpected_error(e, args.verbose)


def daemon_command(action: str) -> None:
    """Manage the Zapio daemon.

    Examples:
        zap daemon status    # Show daemon status
        zap daemon stop      # Stop the daemon
        zap daemon restart   # Restart the daemon
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


def main() -> None:
    """Zapio - Modern embedded build system.

    Replace PlatformIO with URL-based platform/toolchain management.
    """
    parser = argparse.ArgumentParser(
        prog="zap",
        description="Zapio - Modern embedded build system",
    )
    parser.add_argument(
        "--version",
        action="version",
        version="zap 0.1.0",
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
        "-v",
        "--verbose",
        action="store_true",
        help="Show verbose output",
    )

    # Daemon command
    daemon_parser = subparsers.add_parser(
        "daemon",
        help="Manage the Zapio daemon",
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
            verbose=parsed_args.verbose,
        )
        monitor_command(monitor_args)
    elif parsed_args.command == "daemon":
        daemon_command(parsed_args.action)


if __name__ == "__main__":
    main()
