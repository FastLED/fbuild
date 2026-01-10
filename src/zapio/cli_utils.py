"""CLI utility functions for Zapio.

This module provides common utilities used across CLI commands including:
- Environment detection from platformio.ini
- Error handling and formatting
- Monitor argument parsing
"""

import shlex
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

from zapio.config import PlatformIOConfig


@dataclass
class MonitorFlags:
    """Parsed monitor flags from command-line string."""

    timeout: Optional[int] = None
    halt_on_error: Optional[str] = None
    halt_on_success: Optional[str] = None
    baud: int = 115200


class EnvironmentDetector:
    """Handles environment detection from platformio.ini."""

    @staticmethod
    def detect_environment(project_dir: Path, env_name: Optional[str] = None) -> str:
        """Detect or validate environment name from platformio.ini.

        Args:
            project_dir: Project directory containing platformio.ini
            env_name: Optional explicit environment name

        Returns:
            Environment name to use

        Raises:
            FileNotFoundError: If platformio.ini doesn't exist
            ValueError: If no environments found in platformio.ini
        """
        if env_name:
            return env_name

        # Auto-detect environment from platformio.ini
        ini_path = project_dir / "platformio.ini"
        if not ini_path.exists():
            raise FileNotFoundError(f"platformio.ini not found in {project_dir}")

        config = PlatformIOConfig(ini_path)
        detected_env = config.get_default_environment()

        if not detected_env:
            raise ValueError("No environments found in platformio.ini")

        return detected_env


class MonitorFlagParser:
    """Parses monitor flags from command-line strings."""

    @staticmethod
    def parse_monitor_flags(flags_string: str) -> MonitorFlags:
        """Parse monitor flags from a command-line string.

        Args:
            flags_string: String containing monitor flags
                (e.g., "--timeout 60 --halt-on-success 'TEST PASSED'")

        Returns:
            MonitorFlags object with parsed values
        """
        flags = MonitorFlags()
        monitor_args = shlex.split(flags_string)

        i = 0
        while i < len(monitor_args):
            arg = monitor_args[i]
            if arg == "--timeout" and i + 1 < len(monitor_args):
                flags.timeout = int(monitor_args[i + 1])
                i += 2
            elif arg == "--halt-on-error" and i + 1 < len(monitor_args):
                flags.halt_on_error = monitor_args[i + 1]
                i += 2
            elif arg == "--halt-on-success" and i + 1 < len(monitor_args):
                flags.halt_on_success = monitor_args[i + 1]
                i += 2
            elif arg == "--baud" and i + 1 < len(monitor_args):
                flags.baud = int(monitor_args[i + 1])
                i += 2
            else:
                i += 1

        return flags


class ErrorFormatter:
    """Formats and displays error messages with ANSI color codes."""

    # ANSI color codes
    RED = "\033[1;31m"
    GREEN = "\033[1;32m"
    YELLOW = "\033[1;33m"
    RESET = "\033[0m"

    @staticmethod
    def print_error(title: str, message: str, verbose: bool = False) -> None:
        """Print formatted error message.

        Args:
            title: Error title (e.g., "File not found", "Build failed")
            message: Error message details
            verbose: Whether to print verbose output (e.g., traceback)
        """
        print()
        print(f"{ErrorFormatter.RED}✗ {title}{ErrorFormatter.RESET}")
        print()
        print(message)
        print()

    @staticmethod
    def print_success(message: str) -> None:
        """Print formatted success message.

        Args:
            message: Success message
        """
        print()
        print(f"{ErrorFormatter.GREEN}✓ {message}{ErrorFormatter.RESET}")

    @staticmethod
    def print_warning(message: str) -> None:
        """Print formatted warning message.

        Args:
            message: Warning message
        """
        print()
        print(f"{ErrorFormatter.YELLOW}✗ {message}{ErrorFormatter.RESET}")

    @staticmethod
    def handle_file_not_found(error: FileNotFoundError) -> None:
        """Handle FileNotFoundError with standard formatting.

        Args:
            error: The FileNotFoundError to handle
        """
        ErrorFormatter.print_error("Error: File not found", str(error))
        print(
            "Make sure you're in a Zapio project directory with a platformio.ini file."
        )
        sys.exit(1)

    @staticmethod
    def handle_permission_error(error: PermissionError) -> None:
        """Handle PermissionError with standard formatting.

        Args:
            error: The PermissionError to handle
        """
        ErrorFormatter.print_error("Error: Permission denied", str(error))
        sys.exit(1)

    @staticmethod
    def handle_keyboard_interrupt() -> None:
        """Handle KeyboardInterrupt with standard formatting."""
        ErrorFormatter.print_warning("Build interrupted")
        sys.exit(130)  # Standard exit code for SIGINT

    @staticmethod
    def handle_unexpected_error(error: Exception, verbose: bool = False) -> None:
        """Handle unexpected errors with standard formatting.

        Args:
            error: The exception to handle
            verbose: Whether to print traceback
        """
        message = f"{type(error).__name__}: {error}"
        ErrorFormatter.print_error("Unexpected error", message)

        if verbose:
            import traceback

            print("Traceback:")
            print(traceback.format_exc())

        sys.exit(1)


class BannerFormatter:
    """Formats and displays banner messages with borders."""

    DEFAULT_WIDTH = 80
    DEFAULT_BORDER_CHAR = "="

    @staticmethod
    def format_banner(
        message: str,
        width: int = DEFAULT_WIDTH,
        border_char: str = DEFAULT_BORDER_CHAR,
        center: bool = True,
    ) -> str:
        """Format a banner message with top and bottom borders.

        Args:
            message: The message to display (can be multi-line)
            width: Width of the banner in characters (default: 80)
            border_char: Character to use for borders (default: "=")
            center: Whether to center text (default: True)

        Returns:
            Formatted banner string with borders
        """
        lines = message.split("\n")
        border = border_char * width
        formatted_lines = [border]

        for line in lines:
            if center:
                # Center the text
                padding = (width - len(line)) // 2
                formatted_line = " " * padding + line
            else:
                # Left-align with 2-space indent
                formatted_line = "  " + line

            formatted_lines.append(formatted_line)

        formatted_lines.append(border)
        return "\n".join(formatted_lines)

    @staticmethod
    def print_banner(
        message: str,
        width: int = DEFAULT_WIDTH,
        border_char: str = DEFAULT_BORDER_CHAR,
        center: bool = True,
    ) -> None:
        """Print a banner message with top and bottom borders.

        Args:
            message: The message to display (can be multi-line)
            width: Width of the banner in characters (default: 80)
            border_char: Character to use for borders (default: "=")
            center: Whether to center text (default: True)
        """
        print()
        print(
            BannerFormatter.format_banner(
                message, width=width, border_char=border_char, center=center
            )
        )


class PathValidator:
    """Validates project paths and directories."""

    @staticmethod
    def validate_project_dir(project_dir: Path) -> None:
        """Validate that project directory exists and is a directory.

        Args:
            project_dir: Path to validate

        Raises:
            SystemExit: If path doesn't exist or isn't a directory
        """
        if not project_dir.exists():
            print(
                f"{ErrorFormatter.RED}✗ Error: Path does not exist: {project_dir}{ErrorFormatter.RESET}"
            )
            sys.exit(2)
        if not project_dir.is_dir():
            print(
                f"{ErrorFormatter.RED}✗ Error: Path is not a directory: {project_dir}{ErrorFormatter.RESET}"
            )
            sys.exit(2)
