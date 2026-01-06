"""
Command-line interface for Zapio.

This module provides the `zap` CLI tool for building embedded firmware.
"""

import sys
import time
from pathlib import Path
from typing import Optional

import click

from zapio.build import BuildOrchestrator


@click.group()
@click.version_option(version="0.1.0", prog_name="zap")
def main() -> None:
    """Zapio - Modern embedded build system.

    Replace PlatformIO with URL-based platform/toolchain management.
    """
    pass


@main.command()
@click.option(
    "-e",
    "--environment",
    default=None,
    help="Build environment (default: auto-detect from platformio.ini)",
)
@click.option(
    "-c",
    "--clean",
    is_flag=True,
    help="Clean build artifacts before building",
)
@click.option(
    "-v",
    "--verbose",
    is_flag=True,
    help="Show verbose build output",
)
@click.option(
    "-d",
    "--project-dir",
    type=click.Path(exists=True, file_okay=False, dir_okay=True, path_type=Path),
    default=Path.cwd(),
    help="Project directory (default: current directory)",
)
def build(
    environment: Optional[str],
    clean: bool,
    verbose: bool,
    project_dir: Path,
) -> None:
    """Build firmware for embedded target.

    Examples:
        zap build                      # Build default environment
        zap build -e uno              # Build 'uno' environment
        zap build --clean             # Clean build
        zap build --verbose           # Verbose output
        zap build -d /path/to/project # Build specific project
    """
    # Print header
    click.echo("Zapio Build System v0.1.0")
    click.echo()

    try:
        # Create orchestrator
        orchestrator = BuildOrchestrator(verbose=verbose)

        # Determine environment name
        if environment:
            env_name = environment
        else:
            # Auto-detect environment from platformio.ini
            from zapio.config import PlatformIOConfig

            ini_path = project_dir / "platformio.ini"
            if not ini_path.exists():
                raise FileNotFoundError(f"platformio.ini not found in {project_dir}")

            config = PlatformIOConfig(ini_path)
            detected_env = config.get_default_environment()

            if not detected_env:
                raise ValueError("No environments found in platformio.ini")

            env_name = detected_env

        # Show build start message
        if verbose:
            click.echo(f"Building project: {project_dir}")
            click.echo(f"Environment: {env_name}")
            click.echo()
        else:
            click.echo(f"Building environment: {env_name}...")

        # Perform build
        start_time = time.time()
        result = orchestrator.build(
            project_dir=project_dir,
            env_name=env_name,
            clean=clean,
            verbose=verbose,
        )
        build_time = time.time() - start_time

        # Check result
        if result.success:
            # Success output
            click.echo()
            click.secho("✓ Build successful!", fg="green", bold=True)
            click.echo()
            click.echo(f"Firmware: {result.hex_path}")

            # Display size information
            if result.size_info:
                size_info = result.size_info
                click.echo()
                click.echo("Firmware Size:")

                # Program memory (Flash)
                flash_bytes = size_info.total_flash
                if size_info.max_flash:
                    flash_percent = (flash_bytes / size_info.max_flash) * 100
                    click.echo(
                        f"  Program:  {flash_bytes:>6} bytes ({flash_percent:>5.1f}% of {size_info.max_flash} bytes)"
                    )
                else:
                    click.echo(f"  Program:  {flash_bytes:>6} bytes")

                # RAM usage
                ram_bytes = size_info.data + size_info.bss
                if size_info.max_ram:
                    ram_percent = (ram_bytes / size_info.max_ram) * 100
                    click.echo(
                        f"  RAM:      {ram_bytes:>6} bytes ({ram_percent:>5.1f}% of {size_info.max_ram} bytes)"
                    )
                else:
                    click.echo(f"  RAM:      {ram_bytes:>6} bytes")

                click.echo()

            click.echo(f"Build time: {build_time:.2f}s")
            sys.exit(0)
        else:
            # Failure output
            click.echo()
            click.secho("✗ Build failed!", fg="red", bold=True)
            click.echo()
            click.echo(result.message)
            sys.exit(1)

    except FileNotFoundError as e:
        click.echo()
        click.secho("✗ Error: File not found", fg="red", bold=True)
        click.echo()
        click.echo(str(e))
        click.echo()
        click.echo(
            "Make sure you're in a Zapio project directory with a platformio.ini file."
        )
        sys.exit(1)

    except PermissionError as e:
        click.echo()
        click.secho("✗ Error: Permission denied", fg="red", bold=True)
        click.echo()
        click.echo(str(e))
        sys.exit(1)

    except KeyboardInterrupt:
        click.echo()
        click.secho("✗ Build interrupted", fg="yellow", bold=True)
        sys.exit(130)  # Standard exit code for SIGINT

    except Exception as e:
        click.echo()
        click.secho("✗ Unexpected error", fg="red", bold=True)
        click.echo()
        click.echo(f"{type(e).__name__}: {e}")

        if verbose:
            import traceback

            click.echo()
            click.echo("Traceback:")
            click.echo(traceback.format_exc())

        sys.exit(1)


if __name__ == "__main__":
    main()
