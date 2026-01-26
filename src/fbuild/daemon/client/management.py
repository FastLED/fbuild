"""
Daemon Process Management

Handles daemon process discovery, killing, and log tailing.
"""

import time
from pathlib import Path
from typing import Any

import psutil

from .lifecycle import DAEMON_DIR, PID_FILE


def list_all_daemons() -> list[dict[str, Any]]:
    """List all running fbuild daemon instances by scanning processes.

    This function scans all running processes to find fbuild daemons,
    which is useful for detecting multiple daemon instances that may
    have been started due to race conditions or startup errors.

    Returns:
        List of dictionaries with daemon info:
        - pid: Process ID
        - cmdline: Command line arguments
        - uptime: Time since process started (seconds)
        - is_primary: True if this matches the PID file (primary daemon)

    Example:
        >>> daemons = list_all_daemons()
        >>> for d in daemons:
        ...     print(f"PID {d['pid']}: uptime {d['uptime']:.1f}s")
    """
    daemons: list[dict[str, Any]] = []

    # Get primary daemon PID from PID file
    primary_pid = None
    if PID_FILE.exists():
        try:
            with open(PID_FILE) as f:
                primary_pid = int(f.read().strip())
        except (ValueError, OSError):
            pass

    for proc in psutil.process_iter(["pid", "cmdline", "create_time", "name"]):
        try:
            cmdline = proc.info.get("cmdline")
            proc_name = proc.info.get("name", "")
            if not cmdline:
                continue

            # Skip non-Python processes
            if not proc_name.lower().startswith("python"):
                continue

            # Detect fbuild daemon processes
            # Look for patterns like "python daemon.py" in fbuild package
            is_daemon = False

            # Check for direct daemon.py execution from fbuild package
            # Must end with daemon.py and have fbuild in the path
            for arg in cmdline:
                if arg.endswith("daemon.py") and "fbuild" in arg.lower():
                    is_daemon = True
                    break

            # Check for python -m fbuild.daemon.daemon execution
            if not is_daemon and "-m" in cmdline:
                for i, arg in enumerate(cmdline):
                    if arg == "-m" and i + 1 < len(cmdline):
                        module = cmdline[i + 1]
                        if module in ("fbuild.daemon.daemon", "fbuild.daemon"):
                            is_daemon = True
                            break

            if is_daemon:
                pid = proc.info["pid"]
                create_time = proc.info.get("create_time", time.time())
                daemons.append(
                    {
                        "pid": pid,
                        "cmdline": cmdline,
                        "uptime": time.time() - create_time,
                        "is_primary": pid == primary_pid,
                    }
                )

        except (psutil.NoSuchProcess, psutil.AccessDenied, psutil.ZombieProcess):
            continue

    return daemons


def force_kill_daemon(pid: int) -> bool:
    """Force kill a daemon process by PID using SIGKILL.

    This is a forceful termination that doesn't give the daemon
    time to clean up. Use graceful_kill_daemon() when possible.

    Args:
        pid: Process ID to kill

    Returns:
        True if process was killed, False if it didn't exist

    Example:
        >>> if force_kill_daemon(12345):
        ...     print("Daemon killed")
    """
    try:
        proc = psutil.Process(pid)
        proc.kill()  # SIGKILL on Unix, TerminateProcess on Windows
        proc.wait(timeout=5)
        return True
    except psutil.NoSuchProcess:
        return False
    except psutil.TimeoutExpired:
        # Process didn't die even with SIGKILL - unusual but handle it
        return True
    except psutil.AccessDenied:
        print(f"Access denied: cannot kill process {pid}")
        return False


def graceful_kill_daemon(pid: int, timeout: int = 10) -> bool:
    """Gracefully terminate a daemon process with fallback to force kill.

    Sends SIGTERM first to allow cleanup, then SIGKILL if the process
    doesn't exit within the timeout period.

    Args:
        pid: Process ID to terminate
        timeout: Seconds to wait before force killing (default: 10)

    Returns:
        True if process was terminated, False if it didn't exist

    Example:
        >>> if graceful_kill_daemon(12345, timeout=5):
        ...     print("Daemon terminated gracefully")
    """
    try:
        proc = psutil.Process(pid)
        proc.terminate()  # SIGTERM on Unix, TerminateProcess on Windows

        try:
            proc.wait(timeout=timeout)
            return True
        except psutil.TimeoutExpired:
            # Process didn't exit gracefully - force kill
            print(f"Process {pid} didn't exit gracefully, force killing...")
            proc.kill()
            proc.wait(timeout=5)
            return True

    except psutil.NoSuchProcess:
        return False
    except psutil.AccessDenied:
        print(f"Access denied: cannot terminate process {pid}")
        return False


def kill_all_daemons(force: bool = False) -> int:
    """Kill all running daemon instances.

    Useful when multiple daemons have started due to race conditions
    or when the daemon system is in an inconsistent state.

    Args:
        force: If True, use SIGKILL immediately. If False, try SIGTERM first.

    Returns:
        Number of daemons killed

    Example:
        >>> killed = kill_all_daemons(force=False)
        >>> print(f"Killed {killed} daemon(s)")
    """
    killed = 0
    daemons = list_all_daemons()

    if not daemons:
        return 0

    for daemon in daemons:
        pid = daemon["pid"]
        if force:
            if force_kill_daemon(pid):
                killed += 1
                print(f"Force killed daemon (PID {pid})")
        else:
            if graceful_kill_daemon(pid):
                killed += 1
                print(f"Gracefully terminated daemon (PID {pid})")

    # Clean up PID file if we killed any daemons
    if killed > 0 and PID_FILE.exists():
        try:
            PID_FILE.unlink()
        except OSError:
            pass

    return killed


def display_daemon_list() -> None:
    """Display all running daemon instances in a human-readable format."""
    daemons = list_all_daemons()

    if not daemons:
        print("No fbuild daemon instances found")
        return

    print(f"\n=== Running fbuild Daemons ({len(daemons)} found) ===\n")

    for daemon in daemons:
        pid = daemon["pid"]
        uptime = daemon["uptime"]
        is_primary = daemon["is_primary"]

        # Format uptime
        if uptime < 60:
            uptime_str = f"{uptime:.1f}s"
        elif uptime < 3600:
            uptime_str = f"{uptime / 60:.1f}m"
        else:
            uptime_str = f"{uptime / 3600:.1f}h"

        primary_str = " (PRIMARY)" if is_primary else " (ORPHAN)"
        print(f"  PID {pid}: uptime {uptime_str}{primary_str}")

    print()

    # Warn about multiple daemons
    if len(daemons) > 1:
        print("⚠️  Multiple daemon instances detected!")
        print("   This can cause lock conflicts and unexpected behavior.")
        print("   Use 'fbuild daemon kill-all' to clean up, then restart.")
        print()


def tail_daemon_logs(follow: bool = True, lines: int = 50) -> None:
    """Tail the daemon log file.

    This function streams the daemon's log output, allowing users to see
    what the daemon is doing in real-time without affecting its operation.

    Per TASK.md: `fbuild show daemon` should attach to daemon log stream
    and tail it, with exit NOT stopping the daemon.

    Args:
        follow: If True, continuously follow the log file (like tail -f).
                If False, just print the last N lines and exit.
        lines: Number of lines to show initially (default: 50).
    """
    log_file = DAEMON_DIR / "daemon.log"

    if not log_file.exists():
        print("❌ Daemon log file not found")
        print(f"   Expected at: {log_file}")
        print("   Hint: Start the daemon first with 'fbuild build <project>'")
        return

    print(f"📋 Tailing daemon log: {log_file}")
    if follow:
        print("   (Press Ctrl-C to stop viewing - daemon will continue running)\n")
    print("=" * 60)

    try:
        with open(log_file, "r", encoding="utf-8", errors="replace") as f:
            # Read initial lines
            all_lines = f.readlines()

            # Show last N lines
            if len(all_lines) > lines:
                print(f"... (showing last {lines} lines) ...\n")
                for line in all_lines[-lines:]:
                    print(line, end="")
            else:
                for line in all_lines:
                    print(line, end="")

            if not follow:
                return

            # Follow mode - continuously read new content
            while True:
                line = f.readline()
                if line:
                    print(line, end="", flush=True)
                else:
                    # No new content - sleep briefly
                    time.sleep(0.1)

    except KeyboardInterrupt:
        import _thread

        _thread.interrupt_main()
        print("\n\n" + "=" * 60)
        print("✅ Stopped viewing logs (daemon continues running)")
        print("   Use 'fbuild daemon status' to check daemon status")
        print("   Use 'fbuild daemon stop' to stop the daemon")


def get_daemon_log_path() -> Path:
    """Get the path to the daemon log file.

    Returns:
        Path to daemon.log file
    """
    return DAEMON_DIR / "daemon.log"
