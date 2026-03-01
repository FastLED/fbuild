"""
MCP action tools for the fbuild daemon.

These tools have side effects (build, deploy, device refresh, lock release).
All use ToolAnnotations(readOnlyHint=False).
"""

from __future__ import annotations

from mcp.server.fastmcp.exceptions import ToolError
from mcp.types import ToolAnnotations

from fbuild.daemon.mcp import mcp

# ---------------------------------------------------------------------------
# 1. trigger_build — compile a project
# ---------------------------------------------------------------------------


@mcp.tool(annotations=ToolAnnotations(readOnlyHint=False, destructiveHint=False))
def trigger_build(
    project_dir: str,
    environment: str,
    clean: bool,
    verbose: bool,
    jobs: int | None = None,
) -> dict:
    """Trigger a firmware build for a project.

    This blocks until the build completes and returns the result.
    Raises an error if another operation is already in progress.

    Args:
        project_dir: Absolute path to the project directory.
        environment: Build environment name (e.g. "uno", "esp32c6").
        clean: Whether to perform a clean build.
        verbose: Enable verbose compiler output.
        jobs: Number of parallel compilation workers (None = auto).
    """
    from fbuild.daemon.fastapi_app import get_daemon_context
    from fbuild.daemon.messages import BuildRequest
    from fbuild.daemon.processors.build_processor import BuildRequestProcessor

    context = get_daemon_context()

    with context.operation_lock:
        if context.operation_in_progress:
            raise ToolError("Another operation is already in progress. Wait for it to finish or check get_daemon_status().")

    import os

    request = BuildRequest(
        project_dir=project_dir,
        environment=environment,
        clean_build=clean,
        verbose=verbose,
        caller_pid=os.getpid(),
        caller_cwd=os.getcwd(),
        jobs=jobs,
    )

    processor = BuildRequestProcessor()
    success = processor.process_request(request, context)

    return {
        "success": success,
        "request_id": request.request_id,
        "message": "Build successful" if success else "Build failed",
        "exit_code": 0 if success else 1,
    }


# ---------------------------------------------------------------------------
# 2. trigger_deploy — build + flash firmware
# ---------------------------------------------------------------------------


@mcp.tool(annotations=ToolAnnotations(readOnlyHint=False, destructiveHint=True))
def trigger_deploy(
    project_dir: str,
    environment: str,
    port: str | None = None,
    skip_build: bool = False,
) -> dict:
    """Trigger a firmware deploy (build + flash) for a project.

    This blocks until the deploy completes and returns the result.
    Raises an error if another operation is already in progress.

    Args:
        project_dir: Absolute path to the project directory.
        environment: Build environment name.
        port: Serial port (e.g. "COM3"). None for auto-detect.
        skip_build: If True, skip the build step and flash existing firmware.
    """
    from fbuild.daemon.fastapi_app import get_daemon_context
    from fbuild.daemon.messages import DeployRequest
    from fbuild.daemon.processors.deploy_processor import DeployRequestProcessor

    context = get_daemon_context()

    with context.operation_lock:
        if context.operation_in_progress:
            raise ToolError("Another operation is already in progress. Wait for it to finish or check get_daemon_status().")

    import os

    request = DeployRequest(
        project_dir=project_dir,
        environment=environment,
        port=port,
        clean_build=False,
        monitor_after=False,
        monitor_timeout=None,
        monitor_halt_on_error=None,
        monitor_halt_on_success=None,
        monitor_expect=None,
        caller_pid=os.getpid(),
        caller_cwd=os.getcwd(),
        skip_build=skip_build,
    )

    processor = DeployRequestProcessor()
    success = processor.process_request(request, context)

    return {
        "success": success,
        "request_id": request.request_id,
        "message": "Deploy successful" if success else "Deploy failed",
        "exit_code": 0 if success else 1,
    }


# ---------------------------------------------------------------------------
# 3. refresh_devices — re-scan serial ports
# ---------------------------------------------------------------------------


@mcp.tool(annotations=ToolAnnotations(readOnlyHint=False, destructiveHint=False, idempotentHint=True))
def refresh_devices() -> dict:
    """Re-scan serial ports and update the device inventory.

    Returns the list of currently connected devices after the refresh.
    """
    from fbuild.daemon.fastapi_app import get_daemon_context

    context = get_daemon_context()
    devices = context.device_manager.refresh_devices()

    return {
        "device_count": len(devices),
        "devices": [
            {
                "device_id": d.device_id,
                "port": d.port,
                "description": d.description,
                "hwid": d.hwid,
            }
            for d in devices
        ],
    }


# ---------------------------------------------------------------------------
# 4. clear_stale_locks — force-release stuck locks
# ---------------------------------------------------------------------------


@mcp.tool(annotations=ToolAnnotations(readOnlyHint=False, destructiveHint=True, idempotentHint=True))
def clear_stale_locks() -> dict:
    """Force-release any stale (stuck) locks in the daemon.

    A lock is considered stale if it has been held for longer than
    the stale threshold (default 1 hour). Returns counts and details
    of released locks.
    """
    from fbuild.daemon.fastapi_app import get_daemon_context

    context = get_daemon_context()
    lock_manager = context.lock_manager

    stale = lock_manager.get_stale_locks()
    stale_port_ids = [rl.resource_id for rl in stale.stale_port_locks]
    stale_project_ids = [rl.resource_id for rl in stale.stale_project_locks]

    released_count = lock_manager.force_release_stale_locks()

    return {
        "released_count": released_count,
        "stale_port_locks": stale_port_ids,
        "stale_project_locks": stale_project_ids,
    }
