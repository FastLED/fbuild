"""
MCP read-only query tools for the fbuild daemon.

All tools use ToolAnnotations(readOnlyHint=True) and perform deferred imports
inside each function body for isolation.
"""

from __future__ import annotations

import os
import time

from mcp.types import ToolAnnotations

from fbuild import __version__ as APP_VERSION
from fbuild.daemon.mcp import mcp

# ---------------------------------------------------------------------------
# 1. get_daemon_status — PID, uptime, version, port, state
# ---------------------------------------------------------------------------


@mcp.tool(annotations=ToolAnnotations(readOnlyHint=True))
def get_daemon_status() -> dict:
    """Get daemon status including PID, uptime, version, port, and current operation state."""
    from fbuild.daemon.client.http_utils import get_daemon_port
    from fbuild.daemon.fastapi_app import get_daemon_context

    context = get_daemon_context()
    uptime = time.time() - context.daemon_started_at
    dev_mode = os.getenv("FBUILD_DEV_MODE") == "1"

    status = context.status_manager.read_status()

    return {
        "pid": context.daemon_pid,
        "uptime_seconds": round(uptime, 1),
        "version": APP_VERSION,
        "port": get_daemon_port(),
        "dev_mode": dev_mode,
        "state": status.state.value if hasattr(status.state, "value") else str(status.state),
        "message": status.message,
        "operation_in_progress": context.status_manager.get_operation_in_progress(),
    }


# ---------------------------------------------------------------------------
# 2. list_devices — all serial devices with lease info
# ---------------------------------------------------------------------------


@mcp.tool(annotations=ToolAnnotations(readOnlyHint=True))
def list_devices() -> dict:
    """List all serial devices known to the daemon, with lease information."""
    from fbuild.daemon.fastapi_app import get_daemon_context

    context = get_daemon_context()
    device_manager = context.device_manager

    devices_dict = device_manager.get_all_devices()
    devices_list = [{"device_id": device_id, **state.__dict__} for device_id, state in devices_dict.items()]

    return {"device_count": len(devices_list), "devices": devices_list}


# ---------------------------------------------------------------------------
# 3. get_lock_status — port/project locks with counts
# ---------------------------------------------------------------------------


@mcp.tool(annotations=ToolAnnotations(readOnlyHint=True))
def get_lock_status() -> dict:
    """Get active and stale lock information from the daemon."""
    from fbuild.daemon.fastapi_app import get_daemon_context

    context = get_daemon_context()
    lock_manager = context.lock_manager

    status = lock_manager.get_lock_status()
    port_locks = status.get("port_locks", {})
    project_locks = status.get("project_locks", {})

    return {
        "port_locks": port_locks,
        "project_locks": project_locks,
        "active_port_lock_count": len(port_locks),
        "active_project_lock_count": len(project_locks),
    }


# ---------------------------------------------------------------------------
# 4. get_build_queue_status — compilation queue stats
# ---------------------------------------------------------------------------


@mcp.tool(annotations=ToolAnnotations(readOnlyHint=True))
def get_build_queue_status() -> dict:
    """Get compilation queue statistics (pending, active, completed jobs)."""
    from fbuild.daemon.fastapi_app import get_daemon_context

    context = get_daemon_context()
    queue = context.compilation_queue

    stats = queue.get_statistics()
    return {
        "pending": stats.get("pending", 0),
        "running": stats.get("running", 0),
        "completed": stats.get("completed", 0),
        "failed": stats.get("failed", 0),
        "cancelled": stats.get("cancelled", 0),
        "total_jobs": stats.get("total_jobs", 0),
    }


# ---------------------------------------------------------------------------
# 5. get_operation_status — poll a specific operation by ID
# ---------------------------------------------------------------------------


@mcp.tool(annotations=ToolAnnotations(readOnlyHint=True))
def get_operation_status(operation_id: str) -> dict:
    """Get the status of a specific operation by its ID.

    Use this to poll the progress of a build, deploy, or monitor operation.
    """
    from fbuild.daemon.fastapi_app import get_daemon_context

    context = get_daemon_context()
    operation = context.operation_registry.get_operation(operation_id)

    if operation is None:
        return {"found": False, "operation_id": operation_id}

    return {
        "found": True,
        "operation_id": operation.operation_id,
        "type": operation.operation_type.value,
        "state": operation.state.value,
        "project_dir": operation.project_dir,
        "environment": operation.environment,
        "duration_seconds": round(operation.duration() or 0.0, 1),
        "error_message": operation.error_message,
    }


# ---------------------------------------------------------------------------
# 6. get_operation_history — query completed operations
# ---------------------------------------------------------------------------


@mcp.tool(annotations=ToolAnnotations(readOnlyHint=True))
def get_operation_history(project_dir: str | None = None, limit: int | None = None) -> dict:
    """Query recent operations, optionally filtered by project directory.

    Returns operations sorted by creation time (most recent first).
    """
    from fbuild.daemon.fastapi_app import get_daemon_context

    context = get_daemon_context()

    if project_dir is not None:
        operations = context.operation_registry.get_operations_by_project(project_dir)
    else:
        operations = list(context.operation_registry.operations.values())

    # Sort by created_at descending (most recent first)
    operations.sort(key=lambda op: op.created_at, reverse=True)

    if limit is not None:
        operations = operations[:limit]

    return {
        "count": len(operations),
        "operations": [
            {
                "operation_id": op.operation_id,
                "type": op.operation_type.value,
                "state": op.state.value,
                "project_dir": op.project_dir,
                "environment": op.environment,
                "created_at": op.created_at,
                "duration_seconds": round(op.duration() or 0.0, 1),
                "error_message": op.error_message,
            }
            for op in operations
        ],
    }


# ---------------------------------------------------------------------------
# 7. get_build_errors — compilation/linker errors from last build
# ---------------------------------------------------------------------------


@mcp.tool(annotations=ToolAnnotations(readOnlyHint=True))
def get_build_errors(phase: str | None = None) -> dict:
    """Get compilation and linker errors from the most recent build.

    Optionally filter by phase: "download", "compile", "link", or "upload".
    """
    from fbuild.daemon.fastapi_app import get_daemon_context

    context = get_daemon_context()
    collector = context.error_collector

    if phase is not None:
        errors = collector.get_errors_by_phase(phase)
    else:
        errors = collector.get_errors()

    counts = collector.get_error_count()

    return {
        "error_count": counts,
        "summary": collector.format_summary(),
        "errors": [
            {
                "severity": err.severity.value,
                "phase": err.phase,
                "file_path": err.file_path,
                "error_message": err.error_message,
                "stderr": err.stderr,
                "timestamp": err.timestamp,
            }
            for err in errors
        ],
    }


# ---------------------------------------------------------------------------
# 8. get_firmware_status — firmware deployed on a device
# ---------------------------------------------------------------------------


@mcp.tool(annotations=ToolAnnotations(readOnlyHint=True))
def get_firmware_status(port: str) -> dict:
    """Get firmware deployment information for a serial port.

    Returns firmware hash, source hash, project, and whether the firmware
    is considered stale (older than 24 hours).
    """
    from fbuild.daemon.fastapi_app import get_daemon_context

    context = get_daemon_context()
    entry = context.firmware_ledger.get_deployment(port)

    if entry is None:
        return {"found": False, "port": port}

    return {
        "found": True,
        "port": port,
        "firmware_hash": entry.firmware_hash,
        "source_hash": entry.source_hash,
        "project_dir": entry.project_dir,
        "environment": entry.environment,
        "upload_timestamp": entry.upload_timestamp,
        "is_stale": entry.is_stale(),
    }


# ---------------------------------------------------------------------------
# 9. get_connected_clients — who's connected to the daemon
# ---------------------------------------------------------------------------


@mcp.tool(annotations=ToolAnnotations(readOnlyHint=True))
def get_connected_clients() -> dict:
    """Get information about clients currently connected to the daemon."""
    from fbuild.daemon.fastapi_app import get_daemon_context

    context = get_daemon_context()
    clients = context.client_manager.get_all_clients()

    return {
        "client_count": len(clients),
        "clients": [
            {
                "client_id": info.client_id,
                "pid": info.pid,
                "connect_time": info.connect_time,
                "last_heartbeat": info.last_heartbeat,
                "metadata": info.metadata,
                "is_alive": info.is_alive(),
            }
            for info in clients.values()
        ],
    }
