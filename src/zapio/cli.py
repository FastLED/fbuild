"""
Command-line interface for Zapio.

This module provides the `zap` CLI tool for building embedded firmware.
"""

import argparse
import shlex
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

from zapio.build import BuildOrchestrator
from zapio.deploy import Deployer
from zapio.deploy.monitor import SerialMonitor


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
        orchestrator = BuildOrchestrator(verbose=args.verbose)

        # Determine environment name
        if args.environment:
            env_name = args.environment
        else:
            # Auto-detect environment from platformio.ini
            from zapio.config import PlatformIOConfig

            ini_path = args.project_dir / "platformio.ini"
            if not ini_path.exists():
                raise FileNotFoundError(
                    f"platformio.ini not found in {args.project_dir}"
                )

            config = PlatformIOConfig(ini_path)
            detected_env = config.get_default_environment()

            if not detected_env:
                raise ValueError("No environments found in platformio.ini")

            env_name = detected_env

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
            print()
            print("\033[1;32m✓ Build successful!\033[0m")
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
            print()
            print("\033[1;31m✗ Build failed!\033[0m")
            print()
            print(result.message)
            sys.exit(1)

    except FileNotFoundError as e:
        print()
        print("\033[1;31m✗ Error: File not found\033[0m")
        print()
        print(str(e))
        print()
        print(
            "Make sure you're in a Zapio project directory with a platformio.ini file."
        )
        sys.exit(1)

    except PermissionError as e:
        print()
        print("\033[1;31m✗ Error: Permission denied\033[0m")
        print()
        print(str(e))
        sys.exit(1)

    except KeyboardInterrupt:
        print()
        print("\033[1;33m✗ Build interrupted\033[0m")
        sys.exit(130)  # Standard exit code for SIGINT

    except Exception as e:
        print()
        print("\033[1;31m✗ Unexpected error\033[0m")
        print()
        print(f"{type(e).__name__}: {e}")

        if args.verbose:
            import traceback

            print()
            print("Traceback:")
            print(traceback.format_exc())

        sys.exit(1)


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
        if args.environment:
            env_name = args.environment
        else:
            # Auto-detect environment from platformio.ini
            from zapio.config import PlatformIOConfig

            ini_path = args.project_dir / "platformio.ini"
            if not ini_path.exists():
                raise FileNotFoundError(
                    f"platformio.ini not found in {args.project_dir}"
                )

            config = PlatformIOConfig(ini_path)
            detected_env = config.get_default_environment()

            if not detected_env:
                raise ValueError("No environments found in platformio.ini")

            env_name = detected_env

        # If clean flag is set, build first
        if args.clean:
            if args.verbose:
                print(f"Building project: {args.project_dir}")
                print(f"Environment: {env_name}")
                print()
            else:
                print(f"Building environment: {env_name}...")

            orchestrator = BuildOrchestrator(verbose=args.verbose)
            build_result = orchestrator.build(
                project_dir=args.project_dir,
                env_name=env_name,
                clean=True,
                verbose=args.verbose,
            )

            if not build_result.success:
                print()
                print("\033[1;31m✗ Build failed!\033[0m")
                print()
                print(build_result.message)
                sys.exit(1)

            if args.verbose:
                print()
                print("\033[1;32m✓ Build successful!\033[0m")
                print()

        # Create deployer
        deployer = Deployer(verbose=args.verbose)

        # Show deployment start message
        if args.verbose:
            print(f"Deploying project: {args.project_dir}")
            print(f"Environment: {env_name}")
            if args.port:
                print(f"Port: {args.port}")
            print()
        else:
            print(f"Deploying environment: {env_name}...")

        # Perform deployment
        result = deployer.deploy(
            project_dir=args.project_dir,
            env_name=env_name,
            port=args.port,
        )

        # Check result
        if result.success:
            print()
            print("\033[1;32m✓ Deployment successful!\033[0m")
            if result.port:
                print(f"Port: {result.port}")
                deployed_port = result.port
            else:
                deployed_port = args.port

            # If monitor flag is set, start monitoring
            if args.monitor:
                print()
                print("Starting monitor...")
                print()

                # Parse monitor flags
                monitor_args = shlex.split(args.monitor)

                # Build monitor arguments
                mon_timeout = None
                mon_halt_error = None
                mon_halt_success = None
                mon_baud = 115200

                i = 0
                while i < len(monitor_args):
                    arg = monitor_args[i]
                    if arg == "--timeout" and i + 1 < len(monitor_args):
                        mon_timeout = int(monitor_args[i + 1])
                        i += 2
                    elif arg == "--halt-on-error" and i + 1 < len(monitor_args):
                        mon_halt_error = monitor_args[i + 1]
                        i += 2
                    elif arg == "--halt-on-success" and i + 1 < len(monitor_args):
                        mon_halt_success = monitor_args[i + 1]
                        i += 2
                    elif arg == "--baud" and i + 1 < len(monitor_args):
                        mon_baud = int(monitor_args[i + 1])
                        i += 2
                    else:
                        i += 1

                # Start monitor
                mon = SerialMonitor(verbose=args.verbose)
                exit_code = mon.monitor(
                    project_dir=args.project_dir,
                    env_name=env_name,
                    port=deployed_port,
                    baud=mon_baud,
                    timeout=mon_timeout,
                    halt_on_error=mon_halt_error,
                    halt_on_success=mon_halt_success,
                )
                sys.exit(exit_code)

            sys.exit(0)
        else:
            print()
            print("\033[1;31m✗ Deployment failed!\033[0m")
            print()
            print(result.message)
            sys.exit(1)

    except FileNotFoundError as e:
        print()
        print("\033[1;31m✗ Error: File not found\033[0m")
        print()
        print(str(e))
        sys.exit(1)

    except Exception as e:
        print()
        print("\033[1;31m✗ Unexpected error\033[0m")
        print()
        print(f"{type(e).__name__}: {e}")

        if args.verbose:
            import traceback

            print()
            print("Traceback:")
            print(traceback.format_exc())

        sys.exit(1)


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
        # Create monitor
        mon = SerialMonitor(verbose=args.verbose)

        # Determine environment name
        if args.environment:
            env_name = args.environment
        else:
            # Auto-detect environment from platformio.ini
            from zapio.config import PlatformIOConfig

            ini_path = args.project_dir / "platformio.ini"
            if not ini_path.exists():
                raise FileNotFoundError(
                    f"platformio.ini not found in {args.project_dir}"
                )

            config = PlatformIOConfig(ini_path)
            detected_env = config.get_default_environment()

            if not detected_env:
                raise ValueError("No environments found in platformio.ini")

            env_name = detected_env

        # Run monitor
        exit_code = mon.monitor(
            project_dir=args.project_dir,
            env_name=env_name,
            port=args.port,
            baud=args.baud,
            timeout=args.timeout,
            halt_on_error=args.halt_on_error,
            halt_on_success=args.halt_on_success,
        )

        sys.exit(exit_code)

    except FileNotFoundError as e:
        print()
        print("\033[1;31m✗ Error: File not found\033[0m")
        print()
        print(str(e))
        sys.exit(1)

    except Exception as e:
        print()
        print("\033[1;31m✗ Unexpected error\033[0m")
        print()
        print(f"{type(e).__name__}: {e}")

        if args.verbose:
            import traceback

            print()
            print("Traceback:")
            print(traceback.format_exc())

        sys.exit(1)


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

    # Parse arguments
    parsed_args = parser.parse_args()

    # If no command specified, show help
    if not parsed_args.command:
        parser.print_help()
        sys.exit(0)

    # Validate project directory exists
    if hasattr(parsed_args, "project_dir"):
        if not parsed_args.project_dir.exists():
            print(
                f"\033[1;31m✗ Error: Path does not exist: {parsed_args.project_dir}\033[0m"
            )
            sys.exit(2)
        if not parsed_args.project_dir.is_dir():
            print(
                f"\033[1;31m✗ Error: Path is not a directory: {parsed_args.project_dir}\033[0m"
            )
            sys.exit(2)

    # Execute command
    if parsed_args.command == "build":
        args = BuildArgs(
            project_dir=parsed_args.project_dir,
            environment=parsed_args.environment,
            clean=parsed_args.clean,
            verbose=parsed_args.verbose,
        )
        build_command(args)
    elif parsed_args.command == "deploy":
        args = DeployArgs(
            project_dir=parsed_args.project_dir,
            environment=parsed_args.environment,
            port=parsed_args.port,
            clean=parsed_args.clean,
            monitor=parsed_args.monitor,
            verbose=parsed_args.verbose,
        )
        deploy_command(args)
    elif parsed_args.command == "monitor":
        args = MonitorArgs(
            project_dir=parsed_args.project_dir,
            environment=parsed_args.environment,
            port=parsed_args.port,
            baud=parsed_args.baud,
            timeout=parsed_args.timeout,
            halt_on_error=parsed_args.halt_on_error,
            halt_on_success=parsed_args.halt_on_success,
            verbose=parsed_args.verbose,
        )
        monitor_command(args)


if __name__ == "__main__":
    main()
