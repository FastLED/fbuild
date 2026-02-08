"""PlatformIO subprocess runner.

Delegates build/deploy/monitor to the PlatformIO CLI (pio) instead of
fbuild's native orchestrators. Runs synchronously in the CLI process,
bypassing the daemon entirely.

PlatformIO is auto-installed into an isolated venv (via uv-iso-env) on
first use, so users don't need to install it globally.

Environment is sanitized via pio_env.get_pio_safe_env() on Windows/MSYS.
"""

from __future__ import annotations

import os
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Optional

from iso_env import IsoEnv, IsoEnvArgs, Requirements

from fbuild.output import log
from fbuild.pio_env import get_pio_safe_env


def _get_cache_root() -> Path:
    """Determine the fbuild cache root directory.

    Priority: FBUILD_CACHE_DIR > FBUILD_DEV_MODE > default.
    """
    cache_env = os.environ.get("FBUILD_CACHE_DIR")
    if cache_env:
        return Path(cache_env).resolve()
    dev_mode = os.environ.get("FBUILD_DEV_MODE")
    if dev_mode:
        return Path.home() / ".fbuild" / "cache_dev"
    return Path.home() / ".fbuild" / "cache"


def _get_pio_env() -> IsoEnv:
    """Create an IsoEnv for PlatformIO.

    The isolated venv lives under the fbuild cache directory so it
    respects FBUILD_CACHE_DIR / FBUILD_DEV_MODE.

    Returns:
        An IsoEnv configured to run PlatformIO commands.
    """
    venv_path = _get_cache_root() / "pio_iso_env"
    first_install = not venv_path.exists()
    if first_install:
        log("Installing PlatformIO into isolated environment...")
    args = IsoEnvArgs(
        venv_path=venv_path,
        build_info=Requirements("platformio"),
    )
    return IsoEnv(args)


def _platform_subprocess_kwargs() -> dict:
    """Return platform-specific kwargs for subprocess calls.

    On Windows, adds CREATE_NO_WINDOW to prevent console flashing.
    """
    if sys.platform == "win32":
        return {"creationflags": subprocess.CREATE_NO_WINDOW}
    return {}


def run_pio(args: list[str], project_dir: Path, verbose: bool) -> subprocess.CompletedProcess:
    """Run a pio command with sanitized environment.

    Streams stdout/stderr to console in real-time.

    Args:
        args: Arguments to pass to pio (e.g. ["run", "-e", "uno"])
        project_dir: Project directory
        verbose: Whether to show verbose output

    Returns:
        CompletedProcess with exit code
    """
    iso = _get_pio_env()
    cmd = ["pio"] + args

    if verbose:
        log(f"Running: {' '.join(cmd)}")

    env = get_pio_safe_env()

    result = iso.run(
        cmd,
        env=env,
        cwd=str(project_dir),
        stdin=subprocess.DEVNULL,
        check=False,
        **_platform_subprocess_kwargs(),
    )
    return result


def run_pio_with_watchdog(
    args: list[str],
    project_dir: Path,
    verbose: bool,
    inactivity_timeout: int,
) -> int:
    """Run a pio command with a watchdog that kills the process on inactivity.

    Useful for Windows USB upload hangs where pio blocks indefinitely.

    Args:
        args: Arguments to pass to pio
        project_dir: Project directory
        verbose: Whether to show verbose output
        inactivity_timeout: Kill process after this many seconds of no output

    Returns:
        Exit code (0 = success)
    """
    iso = _get_pio_env()
    cmd = ["pio"] + args

    if verbose:
        log(f"Running (watchdog={inactivity_timeout}s): {' '.join(cmd)}")

    env = get_pio_safe_env()

    proc = iso.open_proc(
        cmd,
        env=env,
        cwd=str(project_dir),
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        stdin=subprocess.DEVNULL,
        **_platform_subprocess_kwargs(),
    )

    last_output_time = time.time()
    timed_out = False

    def _reader() -> None:
        nonlocal last_output_time
        assert proc.stdout is not None
        for line in proc.stdout:
            last_output_time = time.time()
            try:
                text = line.decode("utf-8", errors="replace").rstrip("\n\r")
                print(text, flush=True)
            except (ValueError, OSError):
                pass

    reader_thread = threading.Thread(target=_reader, daemon=True)
    reader_thread.start()

    while proc.poll() is None:
        if time.time() - last_output_time > inactivity_timeout:
            timed_out = True
            log(f"Watchdog: no output for {inactivity_timeout}s, killing pio...")
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
            break
        time.sleep(0.5)

    reader_thread.join(timeout=5)

    if timed_out:
        return 1
    return proc.returncode if proc.returncode is not None else 1


def pio_build(
    project_dir: Path,
    environment: str,
    clean: bool,
    verbose: bool,
    jobs: Optional[int],
    has_release: bool,
    has_quick: bool,
) -> bool:
    """Build a project using PlatformIO.

    Maps fbuild flags to pio equivalents. Flags without PIO equivalents
    (--jobs, --release, --quick) are warned about and ignored.

    Args:
        project_dir: Project directory
        environment: PlatformIO environment name
        clean: Whether to clean before building
        verbose: Whether to show verbose output
        jobs: Number of parallel jobs (ignored in PIO mode)
        has_release: Whether --release was explicitly passed
        has_quick: Whether --quick was explicitly passed

    Returns:
        True if build succeeded
    """
    # Warn about ignored flags
    if jobs is not None and jobs != _default_cpu_count():
        log("Note: --jobs is ignored in --platformio mode (PIO manages its own parallelism)")
    if has_release:
        log("Note: --release is ignored in --platformio mode (use platformio.ini build_flags)")
    if has_quick:
        log("Note: --quick is ignored in --platformio mode (use platformio.ini build_flags)")

    # Clean first if requested
    if clean:
        log("Cleaning...")
        result = run_pio(
            ["run", "--target", "clean", "-d", str(project_dir), "-e", environment],
            project_dir=project_dir,
            verbose=verbose,
        )
        if result.returncode != 0:
            log("Clean failed")
            return False

    # Build
    cmd = ["run", "-d", str(project_dir), "-e", environment]
    if verbose:
        cmd.append("-v")

    log(f"Building with PlatformIO (env: {environment})...")
    result = run_pio(cmd, project_dir=project_dir, verbose=verbose)
    return result.returncode == 0


def pio_deploy(
    project_dir: Path,
    environment: str,
    port: Optional[str],
    clean: bool,
    verbose: bool,
    monitor_flags: Optional[str],
) -> bool:
    """Deploy firmware using PlatformIO.

    Args:
        project_dir: Project directory
        environment: PlatformIO environment name
        port: Serial port (None = auto-detect)
        clean: Whether to clean before deploying
        verbose: Whether to show verbose output
        monitor_flags: If not None, chain to pio_monitor after upload.
            Empty string means monitor with defaults.

    Returns:
        True if deploy (and optional monitor) succeeded
    """
    # Clean first if requested
    if clean:
        log("Cleaning...")
        result = run_pio(
            ["run", "--target", "clean", "-d", str(project_dir), "-e", environment],
            project_dir=project_dir,
            verbose=verbose,
        )
        if result.returncode != 0:
            log("Clean failed")
            return False

    # Upload
    cmd = ["run", "--target", "upload", "-d", str(project_dir), "-e", environment]
    if port is not None:
        cmd.extend(["--upload-port", port])
    if verbose:
        cmd.append("-v")

    log(f"Uploading with PlatformIO (env: {environment})...")

    # Use watchdog on Windows to handle USB hangs
    if sys.platform == "win32":
        exit_code = run_pio_with_watchdog(
            cmd,
            project_dir=project_dir,
            verbose=verbose,
            inactivity_timeout=60,
        )
        if exit_code != 0:
            log("Upload failed")
            return False
    else:
        result = run_pio(cmd, project_dir=project_dir, verbose=verbose)
        if result.returncode != 0:
            log("Upload failed")
            return False

    log("Upload complete")

    # Chain to monitor if requested
    if monitor_flags is not None:
        return pio_monitor(
            project_dir=project_dir,
            environment=environment,
            port=port,
            baud=None,
            verbose=verbose,
        )

    return True


def pio_monitor(
    project_dir: Path,
    environment: str,
    port: Optional[str],
    baud: Optional[int],
    verbose: bool,
) -> bool:
    """Monitor serial output using PlatformIO.

    Runs pio device monitor with passthrough (no wrapping).

    Args:
        project_dir: Project directory
        environment: PlatformIO environment name
        port: Serial port (None = auto-detect)
        baud: Baud rate (None = use PIO default from platformio.ini)
        verbose: Whether to show verbose output

    Returns:
        True if monitor exited cleanly
    """
    cmd = ["pio", "device", "monitor", "-d", str(project_dir), "-e", environment]
    if port is not None:
        cmd.extend(["--port", port])
    if baud is not None:
        cmd.extend(["--baud", str(baud)])

    log(f"Monitoring with PlatformIO (env: {environment})...")

    if verbose:
        log(f"Running: {' '.join(cmd)}")

    iso = _get_pio_env()
    env = get_pio_safe_env()

    # stdin=None allows interactive input for monitor (Ctrl-C, etc)
    try:
        result = iso.run(
            cmd,
            env=env,
            cwd=str(project_dir),
            stdin=None,
            check=False,
            **_platform_subprocess_kwargs(),
        )
        return result.returncode == 0
    except KeyboardInterrupt as ke:
        from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

        handle_keyboard_interrupt_properly(ke)


def pio_monitor_wrapped(
    project_dir: Path,
    environment: str,
    port: Optional[str],
    baud: Optional[int],
    verbose: bool,
    timeout: Optional[int],
    halt_on_error: Optional[str],
    halt_on_success: Optional[str],
    expect: Optional[str],
) -> bool:
    """Monitor serial output using PlatformIO with fbuild wrapping.

    Wraps pio device monitor output with fbuild's pattern matching logic
    (timeout, halt-on-error, halt-on-success, expect).

    Args:
        project_dir: Project directory
        environment: PlatformIO environment name
        port: Serial port (None = auto-detect)
        baud: Baud rate (None = use PIO default)
        verbose: Whether to show verbose output
        timeout: Timeout in seconds (None = no timeout)
        halt_on_error: Regex pattern that triggers error exit
        halt_on_success: Regex pattern that triggers success exit
        expect: Regex pattern checked at timeout/success

    Returns:
        True if monitor matched success criteria
    """
    import re

    cmd = ["pio", "device", "monitor", "-d", str(project_dir), "-e", environment]
    if port is not None:
        cmd.extend(["--port", port])
    if baud is not None:
        cmd.extend(["--baud", str(baud)])

    if verbose:
        log(f"Running (wrapped): {' '.join(cmd)}")

    iso = _get_pio_env()
    env = get_pio_safe_env()

    proc = iso.open_proc(
        cmd,
        env=env,
        cwd=str(project_dir),
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        stdin=subprocess.DEVNULL,
        **_platform_subprocess_kwargs(),
    )

    # Compile patterns
    error_re = re.compile(halt_on_error) if halt_on_error else None
    success_re = re.compile(halt_on_success) if halt_on_success else None
    expect_re = re.compile(expect) if expect else None

    start_time = time.time()
    expect_matched = False
    result_success = False

    def _cleanup() -> None:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()

    try:
        assert proc.stdout is not None
        for raw_line in proc.stdout:
            line = raw_line.decode("utf-8", errors="replace").rstrip("\n\r")
            print(line, flush=True)

            # Check timeout
            if timeout is not None and (time.time() - start_time) > timeout:
                log(f"Timeout after {timeout}s")
                if expect_re:
                    result_success = expect_matched
                else:
                    result_success = True  # Timeout without expect = success (completed monitoring)
                _cleanup()
                return result_success

            # Check halt-on-error
            if error_re and error_re.search(line):
                log(f"Halt-on-error matched: {line}")
                _cleanup()
                return False

            # Check halt-on-success
            if success_re and success_re.search(line):
                log(f"Halt-on-success matched: {line}")
                _cleanup()
                return True

            # Track expect pattern
            if expect_re and expect_re.search(line):
                expect_matched = True

    except KeyboardInterrupt as ke:
        _cleanup()
        from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

        handle_keyboard_interrupt_properly(ke)

    # Process ended naturally
    _cleanup()
    if expect_re:
        return expect_matched
    return True


def _default_cpu_count() -> int:
    """Return os.cpu_count() or 1 as fallback."""
    import os

    return os.cpu_count() or 1
