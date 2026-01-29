"""
FastAPI Application for fbuild Daemon

This module provides the FastAPI application for the fbuild daemon, replacing
the file-based IPC with HTTP REST API and WebSocket communication.

Architecture:
- HTTP endpoints for synchronous operations (build, deploy, monitor, etc.)
- WebSocket endpoints for real-time updates and bidirectional communication
- Dependency injection for DaemonContext sharing
- Pydantic models for request/response validation

Usage:
    >>> from fbuild.daemon.fastapi_app import create_app, set_daemon_context
    >>> from fbuild.daemon.daemon_context import create_daemon_context
    >>>
    >>> # Create daemon context
    >>> context = create_daemon_context(...)
    >>> set_daemon_context(context)
    >>>
    >>> # Create FastAPI app
    >>> app = create_app()
    >>>
    >>> # Run with uvicorn
    >>> import uvicorn
    >>> uvicorn.run(app, host="127.0.0.1", port=8765)
"""

from __future__ import annotations

import logging
import os
import time
from contextlib import asynccontextmanager
from typing import TYPE_CHECKING, Any

from fastapi import Depends, FastAPI, HTTPException
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import JSONResponse
from pydantic import BaseModel

from fbuild.daemon.client.http_utils import get_daemon_port

if TYPE_CHECKING:
    from fbuild.daemon.daemon_context import DaemonContext

# Module-level daemon context (set by daemon.py on startup)
_daemon_context: DaemonContext | None = None

# FastAPI app metadata
APP_TITLE = "fbuild Daemon"
APP_VERSION = "1.3.28"
APP_DESCRIPTION = """
fbuild Daemon API - Modern embedded development toolchain daemon.

## Features
- Build, deploy, and monitor operations
- Device lease management
- Lock coordination
- Firmware tracking
- Real-time status updates via WebSocket
"""

# Server configuration
DEFAULT_HOST = "127.0.0.1"


def set_daemon_context(context: DaemonContext) -> None:
    """Set the global daemon context.

    This function is called by daemon.py on startup to make the daemon
    context available to all FastAPI endpoints via dependency injection.

    Args:
        context: The daemon context to set

    Example:
        >>> context = create_daemon_context(...)
        >>> set_daemon_context(context)
    """
    global _daemon_context
    _daemon_context = context
    logging.info("Daemon context set for FastAPI app")


def get_daemon_context() -> DaemonContext:
    """Get the global daemon context (FastAPI dependency).

    Returns:
        The daemon context

    Raises:
        HTTPException: If daemon context not initialized

    Example:
        >>> @app.get("/api/status")
        >>> async def get_status(context: DaemonContext = Depends(get_daemon_context)):
        >>>     return context.status_manager.read_status()
    """
    if _daemon_context is None:
        raise HTTPException(
            status_code=500,
            detail="Daemon context not initialized. This is a server error.",
        )
    return _daemon_context


# Pydantic models for API responses


class HealthResponse(BaseModel):
    """Health check response."""

    status: str
    uptime_seconds: float
    version: str


class DaemonInfoResponse(BaseModel):
    """Daemon information response."""

    pid: int
    started_at: float
    uptime_seconds: float
    version: str
    port: int
    host: str
    dev_mode: bool
    client_count: int
    operation_in_progress: bool


class ErrorResponse(BaseModel):
    """Error response."""

    error: str
    detail: str | None = None


# Lifespan context manager for startup/shutdown


@asynccontextmanager
async def lifespan(app: FastAPI):
    """FastAPI lifespan context manager for startup and shutdown.

    This replaces the deprecated @app.on_event("startup") and
    @app.on_event("shutdown") decorators.

    Yields:
        Control to FastAPI application
    """
    # Startup
    logging.info("FastAPI daemon starting up...")
    logging.info(f"Daemon context: {'initialized' if _daemon_context else 'NOT INITIALIZED'}")

    if _daemon_context:
        logging.info(f"Daemon PID: {_daemon_context.daemon_pid}")
        logging.info(f"Started at: {_daemon_context.daemon_started_at}")

    yield

    # Shutdown
    logging.info("FastAPI daemon shutting down...")
    if _daemon_context:
        from fbuild.daemon.daemon_context import cleanup_daemon_context

        cleanup_daemon_context(_daemon_context)
        logging.info("Daemon context cleaned up")


def register_health_endpoints(app: FastAPI) -> None:
    """Register health check endpoints.

    Args:
        app: FastAPI application instance
    """

    @app.get("/health", response_model=HealthResponse, tags=["Health"])
    async def health_check(context: DaemonContext = Depends(get_daemon_context)) -> HealthResponse:
        """Health check endpoint.

        Returns basic health information about the daemon.
        """
        uptime = time.time() - context.daemon_started_at

        return HealthResponse(
            status="healthy",
            uptime_seconds=uptime,
            version=APP_VERSION,
        )

    @app.get("/", tags=["Health"])
    async def root() -> dict[str, str]:
        """Root endpoint - redirects to docs."""
        return {
            "message": "fbuild Daemon API",
            "version": APP_VERSION,
            "docs": "/docs",
            "health": "/health",
        }


def register_operation_endpoints(app: FastAPI) -> None:
    """Register operation endpoints (build, deploy, monitor, install-deps).

    Args:
        app: FastAPI application instance
    """
    from fbuild.daemon.endpoints.operations import create_operations_router

    operations_router = create_operations_router(get_daemon_context)
    app.include_router(operations_router)


def register_device_endpoints(app: FastAPI) -> None:
    """Register device management endpoints.

    Args:
        app: FastAPI application instance
    """
    from fbuild.daemon.endpoints.devices import router as devices_router

    app.include_router(devices_router)


def register_lock_endpoints(app: FastAPI) -> None:
    """Register lock management endpoints.

    Args:
        app: FastAPI application instance
    """
    from fbuild.daemon.endpoints.locks import router as locks_router

    app.include_router(locks_router)


def register_websocket_endpoints(app: FastAPI) -> None:
    """Register WebSocket endpoints for real-time communication.

    Args:
        app: FastAPI application instance
    """
    from fbuild.daemon.endpoints.websockets import create_websockets_router

    websockets_router = create_websockets_router(get_daemon_context)
    app.include_router(websockets_router)


def register_daemon_endpoints(app: FastAPI) -> None:
    """Register daemon management endpoints.

    Args:
        app: FastAPI application instance
    """

    @app.get("/api/daemon/info", response_model=DaemonInfoResponse, tags=["Daemon"])
    async def get_daemon_info(context: DaemonContext = Depends(get_daemon_context)) -> DaemonInfoResponse:
        """Get daemon information.

        Returns detailed information about the daemon process, including:
        - Process ID
        - Startup time and uptime
        - Version
        - Server configuration
        - Client count
        - Operation status
        """
        uptime = time.time() - context.daemon_started_at
        dev_mode = os.getenv("FBUILD_DEV_MODE") == "1"

        # Get client count from async server if available
        client_count = 0
        if hasattr(context, "async_server") and context.async_server:
            client_count = context.async_server.client_count

        return DaemonInfoResponse(
            pid=context.daemon_pid,
            started_at=context.daemon_started_at,
            uptime_seconds=uptime,
            version=APP_VERSION,
            port=get_daemon_port(),
            host=DEFAULT_HOST,
            dev_mode=dev_mode,
            client_count=client_count,
            operation_in_progress=context.status_manager.get_operation_in_progress(),
        )

    @app.post("/api/daemon/shutdown", tags=["Daemon"])
    async def shutdown_daemon(context: DaemonContext = Depends(get_daemon_context)) -> dict[str, str]:
        """Gracefully shutdown the daemon.

        This endpoint triggers a graceful shutdown of the daemon.
        If an operation is in progress, the shutdown will be refused.

        Returns:
            Shutdown confirmation message
        """
        if context.status_manager.get_operation_in_progress():
            raise HTTPException(
                status_code=409,
                detail="Cannot shutdown while operation is in progress",
            )

        # Create shutdown signal file (legacy mechanism)
        from fbuild.daemon.paths import DAEMON_DIR

        shutdown_file = DAEMON_DIR / "shutdown.signal"
        shutdown_file.touch()

        return {"message": "Shutdown signal sent", "status": "shutting_down"}


def create_app() -> FastAPI:
    """Create and configure the FastAPI application.

    Returns:
        Configured FastAPI application instance
    """
    app = FastAPI(
        title=APP_TITLE,
        version=APP_VERSION,
        description=APP_DESCRIPTION,
        lifespan=lifespan,
        docs_url="/docs",  # Swagger UI
        redoc_url="/redoc",  # ReDoc
        openapi_url="/openapi.json",
    )

    # CORS middleware (localhost only for security)
    app.add_middleware(
        CORSMiddleware,
        allow_origins=["http://localhost", "http://127.0.0.1"],
        allow_credentials=True,
        allow_methods=["*"],
        allow_headers=["*"],
    )

    # Register routes
    register_health_endpoints(app)
    register_daemon_endpoints(app)
    register_operation_endpoints(app)
    register_device_endpoints(app)
    register_lock_endpoints(app)
    register_websocket_endpoints(app)

    return app


# Create the app instance (can be imported by uvicorn)
app = create_app()


# Exception handlers


@app.exception_handler(HTTPException)
async def http_exception_handler(request: Any, exc: HTTPException) -> JSONResponse:
    """Handle HTTP exceptions with structured error response."""
    return JSONResponse(
        status_code=exc.status_code,
        content=ErrorResponse(error=exc.detail or "Unknown error", detail=str(exc)).model_dump(),
    )


@app.exception_handler(Exception)
async def general_exception_handler(request: Any, exc: Exception) -> JSONResponse:
    """Handle general exceptions with structured error response."""
    logging.error(f"Unhandled exception in FastAPI endpoint: {exc}", exc_info=True)
    return JSONResponse(
        status_code=500,
        content=ErrorResponse(error="Internal server error", detail=str(exc)).model_dump(),
    )
