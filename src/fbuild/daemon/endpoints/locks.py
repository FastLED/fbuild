"""
Lock Management Endpoints

Provides HTTP endpoints for lock status queries and stale lock cleanup.
"""

from typing import Any

from fastapi import APIRouter, HTTPException
from pydantic import BaseModel

router = APIRouter(prefix="/api/locks", tags=["locks"])


# Response Models
class LockStatusResponse(BaseModel):
    """Response containing lock status."""

    success: bool
    port_locks: dict[str, Any]
    project_locks: dict[str, Any]
    stale_locks: list[dict[str, Any]]


class ClearLocksResponse(BaseModel):
    """Response from clearing stale locks."""

    success: bool
    cleared_count: int
    message: str


# Endpoints
@router.get("/status", response_model=LockStatusResponse)
async def get_lock_status() -> LockStatusResponse:
    """Get current lock status from the daemon.

    Shows which ports and projects have active locks and who holds them.

    Args:
        lock_manager: Injected ResourceLockManager instance from daemon context.

    Returns:
        Dictionary with lock status information:
        - port_locks: Dict of port -> lock info
        - project_locks: Dict of project -> lock info
        - stale_locks: List of locks that appear to be stale
    """
    from ..fastapi_app import get_daemon_context

    context = get_daemon_context()
    if context.lock_manager is None:
        raise HTTPException(status_code=503, detail="Lock manager not available")
    lock_manager = context.lock_manager

    # Get lock status (returns dict with simple structure)
    status = lock_manager.get_lock_status()

    # Extract stale locks (note: basic get_lock_status() doesn't include stale info)
    stale_locks: list[dict[str, Any]] = []

    # Return simple lock counts
    port_locks = status.get("port_locks", {})
    project_locks = status.get("project_locks", {})

    return LockStatusResponse(success=True, port_locks=port_locks, project_locks=project_locks, stale_locks=stale_locks)


@router.post("/clear", response_model=ClearLocksResponse)
async def clear_stale_locks() -> ClearLocksResponse:
    """Clear stale locks.

    Forces release of any locks that have been held beyond their timeout.
    Useful when operations have hung or crashed without properly releasing locks.

    Args:
        lock_manager: Injected ResourceLockManager instance from daemon context.

    Returns:
        Response with success status and count of cleared locks.
    """
    from ..fastapi_app import get_daemon_context

    context = get_daemon_context()
    if context.lock_manager is None:
        raise HTTPException(status_code=503, detail="Lock manager not available")
    lock_manager = context.lock_manager

    try:
        # Clear stale locks
        cleared_count = lock_manager.force_release_stale_locks()

        if cleared_count > 0:
            message = f"Cleared {cleared_count} stale lock(s)"
        else:
            message = "No stale locks found"

        return ClearLocksResponse(success=True, cleared_count=cleared_count, message=message)

    except KeyboardInterrupt:
        raise
    except Exception as e:
        return ClearLocksResponse(success=False, cleared_count=0, message=f"Error clearing locks: {str(e)}")
