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
from contextvars import ContextVar
from dataclasses import dataclass, replace
from pathlib import Path
from types import TracebackType
from typing import Optional, Protocol, TextIO, runtime_checkable


@dataclass(frozen=True)
class OutputContext:
    """Immutable context for output operations.

    Each build gets its own isolated context, preventing race conditions
    when multiple builds run concurrently.

    This context uses contextvars which:
    - Are thread-safe and async-safe
    - Survive module reloads (stored in interpreter, not module)
    - Provide automatic isolation between concurrent operations
    """

    start_time: Optional[float]
    output_stream: TextIO
    verbose: bool
    output_file: Optional[TextIO]


# Context variable for current output context
_output_context: ContextVar[OutputContext] = ContextVar(
    "output_context",
    default=OutputContext(
        start_time=None,
        output_stream=sys.stdout,
        verbose=True,
        output_file=None,
    ),
)


def get_context() -> OutputContext:
    """Get current output context.

    Returns:
        The current output context for this execution context
    """
    return _output_context.get()


# DEPRECATED: Old module-level globals (for backward compatibility during transition)
# These will be removed in a future version. Use contextvars instead.
# DO NOT use these directly - they are not thread-safe!
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
    ctx = get_context()
    new_ctx = replace(
        ctx,
        start_time=time.time(),
        output_stream=output_stream if output_stream is not None else ctx.output_stream,
    )
    _output_context.set(new_ctx)

    # DEPRECATED: Update old globals for backward compatibility
    global _start_time, _output_stream
    _start_time = new_ctx.start_time
    if output_stream is not None:
        _output_stream = output_stream


def reset_timer() -> None:
    """
    Reset the timer to current time.

    Useful for resetting the epoch at the start of a new build phase.
    """
    ctx = get_context()
    new_ctx = replace(ctx, start_time=time.time())
    _output_context.set(new_ctx)

    # DEPRECATED: Update old globals for backward compatibility
    global _start_time
    _start_time = new_ctx.start_time


def set_verbose(verbose: bool) -> None:
    """
    Set verbose mode for logging.

    Args:
        verbose: If True, all messages are printed. If False, only non-verbose messages.
    """
    ctx = get_context()
    new_ctx = replace(ctx, verbose=verbose)
    _output_context.set(new_ctx)

    # DEPRECATED: Update old globals for backward compatibility
    global _verbose
    _verbose = verbose


def set_output_file(output_file: Optional[TextIO]) -> None:
    """
    Set a file to receive all log output (in addition to stdout).

    Args:
        output_file: File object to receive output, or None to disable file output
    """
    ctx = get_context()
    new_ctx = replace(ctx, output_file=output_file)
    _output_context.set(new_ctx)

    # DEPRECATED: Update old globals for backward compatibility
    global _output_file
    _output_file = output_file


def get_output_file() -> Optional[TextIO]:
    """
    Get the current output file.

    Returns:
        The current output file, or None if not set
    """
    ctx = get_context()
    return ctx.output_file


def get_elapsed() -> float:
    """
    Get elapsed time since timer initialization.

    Returns:
        Elapsed time in seconds
    """
    ctx = get_context()
    if ctx.start_time is None:
        # Auto-initialize if not set
        init_timer()
        ctx = get_context()
    assert ctx.start_time is not None  # For type checker
    return time.time() - ctx.start_time


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
    ctx = get_context()
    timestamp = format_timestamp()
    line = f"{timestamp} {message}{end}"

    try:
        ctx.output_stream.write(line)
        ctx.output_stream.flush()
    except (ValueError, OSError):
        # Ignore if stream is closed (e.g., in test environment)
        pass

    # Also write to output file if set
    if ctx.output_file is not None:
        try:
            ctx.output_file.write(line)
            ctx.output_file.flush()
        except (ValueError, OSError):
            # Ignore if file is closed
            pass


def log(message: str, verbose_only: bool = False) -> None:
    """
    Log a message with timestamp.

    Args:
        message: Message to log
        verbose_only: If True, only print if verbose mode is enabled
    """
    ctx = get_context()
    if verbose_only and not ctx.verbose:
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
    ctx = get_context()
    if verbose_only and not ctx.verbose:
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
    ctx = get_context()
    if verbose_only and not ctx.verbose:
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
    ctx = get_context()
    if verbose_only and not ctx.verbose:
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
    ctx = get_context()
    if verbose_only and not ctx.verbose:
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
    ctx = get_context()
    if verbose_only and not ctx.verbose:
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
    ctx = get_context()
    if verbose_only and not ctx.verbose:
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


# =============================================================================
# Progress Callback Protocol and Helpers
# =============================================================================


def format_size(size_bytes: int) -> str:
    """
    Format a byte count as a human-readable size.

    Args:
        size_bytes: Size in bytes

    Returns:
        Human-readable size string (e.g., "1.2 MB", "340 KB", "12 B")
    """
    if size_bytes < 0:
        return "0 B"
    if size_bytes < 1024:
        return f"{size_bytes} B"
    elif size_bytes < 1024 * 1024:
        return f"{size_bytes / 1024:.1f} KB"
    elif size_bytes < 1024 * 1024 * 1024:
        return f"{size_bytes / (1024 * 1024):.1f} MB"
    else:
        return f"{size_bytes / (1024 * 1024 * 1024):.1f} GB"


def format_progress_bar(current: int, total: int, width: int = 28) -> str:
    """
    Generate an ASCII progress bar.

    Args:
        current: Current progress value
        total: Total progress value
        width: Width of the progress bar in characters (default 28)

    Returns:
        ASCII progress bar string like "████████░░░░░░░░░░░░░░░░░░░░"

    Examples:
        >>> format_progress_bar(5, 10, 20)
        '██████████░░░░░░░░░░'
        >>> format_progress_bar(0, 10, 10)
        '░░░░░░░░░░'
        >>> format_progress_bar(10, 10, 10)
        '██████████'
    """
    if total <= 0:
        return "░" * width
    ratio = min(1.0, max(0.0, current / total))
    filled = int(width * ratio)
    empty = width - filled
    return "█" * filled + "░" * empty


@runtime_checkable
class ProgressCallback(Protocol):
    """
    Protocol for progress callback implementations.

    This protocol defines the interface for tracking build progress events.
    Implementations can log to terminal, update TUI elements, or report to daemons.
    """

    def on_file_start(self, file: str, index: int, total: int) -> None:
        """
        Called when compilation of a file starts.

        Args:
            file: Name or path of the file being compiled
            index: Current file index (1-based)
            total: Total number of files to compile
        """
        ...

    def on_file_complete(self, file: str, index: int, total: int, cached: bool = False) -> None:
        """
        Called when compilation of a file completes.

        Args:
            file: Name or path of the file that was compiled
            index: Current file index (1-based)
            total: Total number of files to compile
            cached: If True, the file was served from cache (not recompiled)
        """
        ...

    def on_download_progress(self, url: str, downloaded: int, total: int) -> None:
        """
        Called during file download to report progress.

        Args:
            url: URL being downloaded
            downloaded: Bytes downloaded so far
            total: Total bytes to download (0 if unknown)
        """
        ...

    def on_extract_progress(self, archive: str, extracted: int, total: int) -> None:
        """
        Called during archive extraction to report progress.

        Args:
            archive: Archive file being extracted
            extracted: Number of files/items extracted so far
            total: Total number of files/items to extract
        """
        ...

    def on_phase_start(self, phase: int, total: int, message: str) -> None:
        """
        Called when a build phase starts.

        Args:
            phase: Current phase number (1-based)
            total: Total number of phases
            message: Phase description
        """
        ...

    def on_phase_complete(self, phase: int, total: int, elapsed: float) -> None:
        """
        Called when a build phase completes.

        Args:
            phase: Completed phase number (1-based)
            total: Total number of phases
            elapsed: Time spent in this phase (seconds)
        """
        ...


class DefaultProgressCallback:
    """
    Default implementation of ProgressCallback that logs to the existing output system.

    This implementation uses log_detail() and log_phase() to display progress
    in a format consistent with the rest of fbuild's output.
    """

    def __init__(self, verbose_only: bool = False, show_progress_bar: bool = True):
        """
        Initialize the default progress callback.

        Args:
            verbose_only: If True, only log when verbose mode is enabled
            show_progress_bar: If True, show ASCII progress bars for downloads/extractions
        """
        self.verbose_only = verbose_only
        self.show_progress_bar = show_progress_bar
        self._last_download_percent: int = -1
        self._last_extract_percent: int = -1

    def on_file_start(self, file: str, index: int, total: int) -> None:
        """Log file compilation start."""
        log_detail(f"[{index}/{total}] Compiling {file}...", verbose_only=self.verbose_only)

    def on_file_complete(self, file: str, index: int, total: int, cached: bool = False) -> None:
        """Log file compilation completion."""
        suffix = " (cached)" if cached else ""
        log_detail(f"[{index}/{total}] {file}{suffix}", verbose_only=True)

    def on_download_progress(self, url: str, downloaded: int, total: int) -> None:
        """Log download progress (rate-limited to avoid excessive output)."""
        if total <= 0:
            return

        percent = int(100 * downloaded / total)
        # Only log at 10% intervals to avoid spam
        if percent // 10 == self._last_download_percent // 10 and percent != 100:
            return
        self._last_download_percent = percent

        filename = url.split("/")[-1][:30]
        if self.show_progress_bar:
            bar = format_progress_bar(downloaded, total, 20)
            log_detail(f"{bar} {percent:3d}% {filename} ({format_size(downloaded)}/{format_size(total)})", verbose_only=self.verbose_only)
        else:
            log_detail(f"Downloading {filename}: {percent}% ({format_size(downloaded)}/{format_size(total)})", verbose_only=self.verbose_only)

    def on_extract_progress(self, archive: str, extracted: int, total: int) -> None:
        """Log extraction progress (rate-limited to avoid excessive output)."""
        if total <= 0:
            return

        percent = int(100 * extracted / total)
        # Only log at 25% intervals to avoid spam
        if percent // 25 == self._last_extract_percent // 25 and percent != 100:
            return
        self._last_extract_percent = percent

        archive_name = Path(archive).name[:30]
        if self.show_progress_bar:
            bar = format_progress_bar(extracted, total, 20)
            log_detail(f"{bar} {percent:3d}% Extracting {archive_name} ({extracted}/{total} files)", verbose_only=self.verbose_only)
        else:
            log_detail(f"Extracting {archive_name}: {percent}% ({extracted}/{total} files)", verbose_only=self.verbose_only)

    def on_phase_start(self, phase: int, total: int, message: str) -> None:
        """Log phase start using log_phase."""
        log_phase(phase, total, f"{message}...", verbose_only=self.verbose_only)

    def on_phase_complete(self, phase: int, total: int, elapsed: float) -> None:
        """Log phase completion with elapsed time."""
        del phase, total  # Unused in default implementation
        log_detail(f"Done ({elapsed:.2f}s)", verbose_only=self.verbose_only)
