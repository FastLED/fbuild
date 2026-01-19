"""
Centralized logging and output module for fbuild.

This module provides timestamped output from program launch to help audit
where time is spent during builds. All output is prefixed with elapsed time
in MM:SS.cc format (minutes:seconds.centiseconds).

Example output:
    00:00.12 fbuild Build System v1.2.4
    00:00.15 Building environment: uno...
    00:01.23 [1/9] Parsing platformio.ini...
    00:01.45      Board: Arduino Uno
    00:02.67      MCU: atmega328p

Usage:
    from fbuild.output import log, log_phase, log_detail, init_timer

    # Initialize at program start (done automatically on first use)
    init_timer()

    # Log a message with timestamp
    log("Building environment: uno...")

    # Log a build phase
    log_phase(1, 9, "Parsing platformio.ini...")

    # Log a detail (indented)
    log_detail("Board: Arduino Uno")
"""

import sys
import time
from pathlib import Path
from types import TracebackType
from typing import Optional, TextIO

# Global state for the timer
_start_time: Optional[float] = None
_output_stream: TextIO = sys.stdout
_verbose: bool = True
_output_file: Optional[TextIO] = None


def init_timer(output_stream: Optional[TextIO] = None) -> None:
    """
    Initialize the program timer.

    Call this at program startup to set the reference time for all timestamps.
    If not called explicitly, it will be called automatically on first log.

    Args:
        output_stream: Optional output stream (defaults to sys.stdout)
    """
    global _start_time, _output_stream
    _start_time = time.time()
    if output_stream is not None:
        _output_stream = output_stream


def reset_timer() -> None:
    """
    Reset the timer to current time.

    Useful for resetting the epoch at the start of a new build phase.
    """
    global _start_time
    _start_time = time.time()


def set_verbose(verbose: bool) -> None:
    """
    Set verbose mode for logging.

    Args:
        verbose: If True, all messages are printed. If False, only non-verbose messages.
    """
    global _verbose
    _verbose = verbose


def set_output_file(output_file: Optional[TextIO]) -> None:
    """
    Set a file to receive all log output (in addition to stdout).

    Args:
        output_file: File object to receive output, or None to disable file output
    """
    global _output_file
    _output_file = output_file


def get_output_file() -> Optional[TextIO]:
    """
    Get the current output file.

    Returns:
        The current output file, or None if not set
    """
    return _output_file


def get_elapsed() -> float:
    """
    Get elapsed time since timer initialization.

    Returns:
        Elapsed time in seconds
    """
    global _start_time
    if _start_time is None:
        init_timer()
    return time.time() - _start_time  # type: ignore


def format_timestamp() -> str:
    """
    Format the current elapsed time as MM:SS.cc.

    Returns:
        Formatted timestamp string
    """
    elapsed = get_elapsed()
    minutes = int(elapsed // 60)
    seconds = elapsed % 60
    return f"{minutes:02d}:{seconds:05.2f}"


def _print(message: str, end: str = "\n") -> None:
    """
    Internal print function with timestamp.

    Args:
        message: Message to print
        end: End character (default newline)
    """
    timestamp = format_timestamp()
    line = f"{timestamp} {message}{end}"
    _output_stream.write(line)
    _output_stream.flush()

    # Also write to output file if set
    if _output_file is not None:
        _output_file.write(line)
        _output_file.flush()


def log(message: str, verbose_only: bool = False) -> None:
    """
    Log a message with timestamp.

    Args:
        message: Message to log
        verbose_only: If True, only print if verbose mode is enabled
    """
    global _verbose
    if verbose_only and not _verbose:
        return
    _print(message)


def log_phase(phase: int, total: int, message: str, verbose_only: bool = False) -> None:
    """
    Log a build phase message.

    Format: [N/M] message

    Args:
        phase: Current phase number
        total: Total number of phases
        message: Phase description
        verbose_only: If True, only print if verbose mode is enabled
    """
    global _verbose
    if verbose_only and not _verbose:
        return
    _print(f"[{phase}/{total}] {message}")


def log_detail(message: str, indent: int = 6, verbose_only: bool = False) -> None:
    """
    Log a detail message (indented).

    Args:
        message: Detail message
        indent: Number of spaces to indent (default 6)
        verbose_only: If True, only print if verbose mode is enabled
    """
    global _verbose
    if verbose_only and not _verbose:
        return
    _print(f"{' ' * indent}{message}")


def log_file(source_type: str, filename: str, cached: bool = False, verbose_only: bool = True) -> None:
    """
    Log a file compilation message.

    Format: [source_type] filename (cached)

    Args:
        source_type: Type of source (e.g., 'sketch', 'core', 'variant')
        filename: Name of the file
        cached: If True, append "(cached)" to message
        verbose_only: If True, only print if verbose mode is enabled
    """
    global _verbose
    if verbose_only and not _verbose:
        return
    suffix = " (cached)" if cached else ""
    _print(f"      [{source_type}] {filename}{suffix}")


def log_header(title: str, version: str) -> None:
    """
    Log a header message (e.g., program startup).

    Args:
        title: Program title
        version: Version string
    """
    _print(f"{title} v{version}")
    _print("")


def log_size_info(
    program_bytes: int,
    program_percent: Optional[float],
    max_flash: Optional[int],
    data_bytes: int,
    bss_bytes: int,
    ram_bytes: int,
    ram_percent: Optional[float],
    max_ram: Optional[int],
    verbose_only: bool = False,
) -> None:
    """
    Log firmware size information.

    Args:
        program_bytes: Program flash usage in bytes
        program_percent: Percentage of flash used (or None)
        max_flash: Maximum flash size (or None)
        data_bytes: Data section size in bytes
        bss_bytes: BSS section size in bytes
        ram_bytes: Total RAM usage in bytes
        ram_percent: Percentage of RAM used (or None)
        max_ram: Maximum RAM size (or None)
        verbose_only: If True, only print if verbose mode is enabled
    """
    global _verbose
    if verbose_only and not _verbose:
        return

    _print("Firmware Size:")

    if program_percent is not None and max_flash is not None:
        _print(f"  Program:  {program_bytes:6d} bytes ({program_percent:5.1f}% of {max_flash} bytes)")
    else:
        _print(f"  Program:  {program_bytes:6d} bytes")

    _print(f"  Data:     {data_bytes:6d} bytes")
    _print(f"  BSS:      {bss_bytes:6d} bytes")

    if ram_percent is not None and max_ram is not None:
        _print(f"  RAM:      {ram_bytes:6d} bytes ({ram_percent:5.1f}% of {max_ram} bytes)")
    else:
        _print(f"  RAM:      {ram_bytes:6d} bytes")


def log_build_complete(build_time: float, verbose_only: bool = False) -> None:
    """
    Log build completion message.

    Args:
        build_time: Total build time in seconds
        verbose_only: If True, only print if verbose mode is enabled
    """
    global _verbose
    if verbose_only and not _verbose:
        return
    _print("")
    _print(f"Build time: {build_time:.2f}s")


def log_error(message: str) -> None:
    """
    Log an error message.

    Args:
        message: Error message
    """
    _print(f"ERROR: {message}")


def log_warning(message: str) -> None:
    """
    Log a warning message.

    Args:
        message: Warning message
    """
    _print(f"WARNING: {message}")


def log_success(message: str) -> None:
    """
    Log a success message.

    Args:
        message: Success message
    """
    _print(message)


def log_firmware_path(path: Path, verbose_only: bool = False) -> None:
    """
    Log firmware output path.

    Args:
        path: Path to firmware file
        verbose_only: If True, only print if verbose mode is enabled
    """
    global _verbose
    if verbose_only and not _verbose:
        return
    log_detail(f"Firmware: {path}")


class TimedLogger:
    """
    Context manager for logging with elapsed time tracking.

    Usage:
        with TimedLogger("Compiling sources") as logger:
            # Do compilation
            logger.detail("Compiled 10 files")
        # Automatically logs completion time
    """

    def __init__(self, operation: str, phase: Optional[tuple[int, int]] = None, verbose_only: bool = False):
        """
        Initialize timed logger.

        Args:
            operation: Description of the operation
            phase: Optional (current, total) phase numbers
            verbose_only: If True, only print if verbose mode is enabled
        """
        self.operation = operation
        self.phase = phase
        self.verbose_only = verbose_only
        self.start_time = 0.0

    def __enter__(self) -> "TimedLogger":
        self.start_time = time.time()
        if self.phase:
            log_phase(self.phase[0], self.phase[1], f"{self.operation}...", self.verbose_only)
        else:
            log(f"{self.operation}...", self.verbose_only)
        return self

    def __exit__(
        self,
        exc_type: Optional[type[BaseException]],
        exc_val: Optional[BaseException],
        exc_tb: Optional[TracebackType],
    ) -> None:
        del exc_val, exc_tb  # Unused
        elapsed = time.time() - self.start_time
        if exc_type is None:
            log_detail(f"Done ({elapsed:.2f}s)", verbose_only=self.verbose_only)
        return None

    def detail(self, message: str) -> None:
        """Log a detail message within this operation."""
        log_detail(message, verbose_only=self.verbose_only)

    def log(self, message: str) -> None:
        """Log a message within this operation."""
        log(message, self.verbose_only)
