"""
Daemon Management Endpoints

Provides HTTP endpoints for daemon lifecycle management and status.
"""

import time

from fastapi import APIRouter
from pydantic import BaseModel

router = APIRouter(prefix="/api/daemon", tags=["daemon"])


# Response Models
class DaemonInfoResponse(BaseModel):
    """Extended daemon information."""

    pid: int
    uptime: float
    status: str
    version: str
    cache_dir: str | None = None
    daemon_dir: str | None = None


class ShutdownResponse(BaseModel):
    """Response from shutdown request."""

    success: bool
    message: str


# Endpoints
@router.get("/info", response_model=DaemonInfoResponse)
async def get_daemon_info() -> DaemonInfoResponse:
    """Get detailed daemon information.

    Returns:
        Daemon info including PID, uptime, version, and paths.
    """
    from ..fastapi_app import get_daemon_context

    context = get_daemon_context()
    if context is None:
        # Daemon not fully initialized yet
        import os

        return DaemonInfoResponse(
            pid=os.getpid(),
            uptime=0.0,
            status="initializing",
            version="unknown",
        )

    uptime = time.time() - context.daemon_started_at

    # Get version
    try:
        from fbuild import __version__

        version = __version__
    except ImportError:
        version = "unknown"

    # Get paths (not tracked in context, use None)
    cache_dir = None
    daemon_dir = None

    return DaemonInfoResponse(
        pid=context.daemon_pid,
        uptime=uptime,
        status="running",
        version=version,
        cache_dir=cache_dir,
        daemon_dir=daemon_dir,
    )


@router.post("/shutdown", response_model=ShutdownResponse)
async def shutdown_daemon() -> ShutdownResponse:
    """Gracefully shutdown the daemon.

    This endpoint triggers a graceful shutdown of the daemon.
    All active operations will be allowed to complete before shutdown.

    Returns:
        Response with success status.
    """
    from ..fastapi_app import get_daemon_context

    context = get_daemon_context()
    if context is None:
        return ShutdownResponse(success=False, message="Daemon context not available")

    # Create shutdown signal file (daemon main loop will detect and exit)
    from ..paths import DAEMON_DIR

    shutdown_file = DAEMON_DIR / "shutdown.signal"
    shutdown_file.touch()

    return ShutdownResponse(success=True, message="Shutdown initiated")
