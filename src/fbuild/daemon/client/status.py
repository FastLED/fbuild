"""
Daemon Status Display and Monitoring

Handles reading daemon status, displaying progress updates, and formatting status information.
"""

import json
import time
from typing import Any

from fbuild.daemon.messages import DaemonState, DaemonStatus
from fbuild.daemon.paths import PID_FILE, STATUS_FILE

# Spinner characters for progress indication
SPINNER_CHARS = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]


def read_status_file() -> DaemonStatus:
    """Read current daemon status with corruption recovery.

    Returns:
        DaemonStatus object (or default status if file doesn't exist or corrupted)
    """
    if not STATUS_FILE.exists():
        return DaemonStatus(
            state=DaemonState.UNKNOWN,
            message="Status file not found",
            updated_at=time.time(),
        )

    try:
        with open(STATUS_FILE) as f:
            data = json.load(f)

        # Parse into typed DaemonStatus
        return DaemonStatus.from_dict(data)

    except (json.JSONDecodeError, ValueError):
        # Corrupted JSON - return default status
        return DaemonStatus(
            state=DaemonState.UNKNOWN,
            message="Status file corrupted (invalid JSON)",
            updated_at=time.time(),
        )
    except KeyboardInterrupt:
        import _thread

        _thread.interrupt_main()
        raise
    except Exception:
        return DaemonStatus(
            state=DaemonState.UNKNOWN,
            message="Failed to read status",
            updated_at=time.time(),
        )


def display_status(status: DaemonStatus, prefix: str = "  ") -> None:
    """Display status update to user.

    Args:
        status: DaemonStatus object
        prefix: Line prefix for indentation
    """
    # Show current operation if available, otherwise use message
    display_text = status.current_operation or status.message

    if status.state == DaemonState.DEPLOYING:
        print(f"{prefix}📦 {display_text}", flush=True)
    elif status.state == DaemonState.MONITORING:
        print(f"{prefix}👁️  {display_text}", flush=True)
    elif status.state == DaemonState.BUILDING:
        print(f"{prefix}🔨 {display_text}", flush=True)
    elif status.state == DaemonState.COMPLETED:
        print(f"{prefix}✅ {display_text}", flush=True)
    elif status.state == DaemonState.FAILED:
        print(f"{prefix}❌ {display_text}", flush=True)
    else:
        print(f"{prefix}ℹ️  {display_text}", flush=True)


def display_spinner_progress(
    status: DaemonStatus,
    elapsed: float,
    spinner_idx: int,
    prefix: str = "  ",
) -> None:
    """Display spinner with elapsed time when status hasn't changed.

    Uses carriage return to update in place without new line.

    Args:
        status: DaemonStatus object
        elapsed: Elapsed time in seconds
        spinner_idx: Current spinner index
        prefix: Line prefix for indentation
    """
    spinner = SPINNER_CHARS[spinner_idx % len(SPINNER_CHARS)]
    display_text = status.current_operation or status.message

    # Format elapsed time
    mins = int(elapsed) // 60
    secs = int(elapsed) % 60
    if mins > 0:
        time_str = f"{mins}m {secs}s"
    else:
        time_str = f"{secs}s"

    # Use carriage return to update in place
    print(f"\r{prefix}{spinner} {display_text} ({time_str})", end="", flush=True)


def get_daemon_status() -> dict[str, Any]:
    """Get current daemon status.

    Returns:
        Dictionary with daemon status information
    """
    from fbuild.daemon.client.lifecycle import is_daemon_running

    status: dict[str, Any] = {
        "running": is_daemon_running(),
        "pid_file_exists": PID_FILE.exists(),
        "status_file_exists": STATUS_FILE.exists(),
    }

    if PID_FILE.exists():
        try:
            with open(PID_FILE) as f:
                status["pid"] = int(f.read().strip())
        except KeyboardInterrupt:
            import _thread

            _thread.interrupt_main()
            raise
        except Exception:
            status["pid"] = None

    if STATUS_FILE.exists():
        daemon_status = read_status_file()
        # Convert DaemonStatus to dict for JSON serialization
        status["current_status"] = daemon_status.to_dict()

    # Fetch mcp_url from daemon HTTP API when running
    if status["running"]:
        try:
            from fbuild.daemon.client.http_utils import get_daemon_url, http_client

            with http_client(timeout=2.0, connect_timeout=1.0) as client:
                resp = client.get(get_daemon_url("/api/daemon/info"))
                if resp.status_code == 200:
                    info = resp.json()
                    if info.get("mcp_url"):
                        status["mcp_url"] = info["mcp_url"]
        except KeyboardInterrupt:
            raise
        except Exception:
            pass

    return status


def display_daemon_stats_compact() -> None:
    """Display daemon stats in a compact single-line format.

    This function is called immediately when the client starts to show
    the current daemon status. It's designed to be non-intrusive.
    """
    from fbuild.daemon.client.lifecycle import is_daemon_running

    if not is_daemon_running():
        print("🔴 Daemon: not running")
        return

    status = read_status_file()

    # Calculate uptime if daemon_started_at is available
    uptime_str = ""
    if status.daemon_started_at:
        uptime = time.time() - status.daemon_started_at
        if uptime < 60:
            uptime_str = f"{uptime:.0f}s"
        elif uptime < 3600:
            uptime_str = f"{uptime / 60:.0f}m"
        else:
            uptime_str = f"{uptime / 3600:.1f}h"

    # Build the status line
    pid_str = f"PID {status.daemon_pid}" if status.daemon_pid else ""
    state_emoji = {
        DaemonState.IDLE: "🟢",
        DaemonState.BUILDING: "🔨",
        DaemonState.DEPLOYING: "📦",
        DaemonState.MONITORING: "👁️",
        DaemonState.COMPLETED: "✅",
        DaemonState.FAILED: "❌",
        DaemonState.UNKNOWN: "❓",
    }.get(status.state, "❓")

    # Count active locks
    active_port_locks = 0
    if status.locks:
        active_port_locks = sum(1 for info in status.locks.port_locks.values() if isinstance(info, dict) and info.get("is_held"))

    # Build compact stats line
    parts = [f"{state_emoji} Daemon: {status.state.value}"]
    if pid_str:
        parts.append(pid_str)
    if uptime_str:
        parts.append(f"up {uptime_str}")
    if active_port_locks > 0:
        parts.append(f"locks: {active_port_locks}")
    if status.operation_in_progress:
        op_info = status.current_operation or status.message
        if op_info and len(op_info) > 30:
            op_info = op_info[:27] + "..."
        parts.append(f"[{op_info}]")

    print(" | ".join(parts))
