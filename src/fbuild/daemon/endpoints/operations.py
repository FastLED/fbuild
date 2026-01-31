"""
FastAPI endpoints for build, deploy, monitor, and install-deps operations.

This module provides HTTP endpoints for the core fbuild operations.
The processors run synchronously in a thread pool to avoid blocking
the FastAPI event loop.
"""

from __future__ import annotations

import asyncio
import logging
from concurrent.futures import ThreadPoolExecutor
from typing import TYPE_CHECKING, Any

from fastapi import APIRouter, Depends
from pydantic import BaseModel, Field

from fbuild.daemon.messages import (
    BuildRequest,
    DeployRequest,
    InstallDependenciesRequest,
    MonitorRequest,
)

if TYPE_CHECKING:
    from fbuild.daemon.daemon_context import DaemonContext

# Thread pool for running synchronous processors
_thread_pool: ThreadPoolExecutor | None = None


def get_thread_pool() -> ThreadPoolExecutor:
    """Get or create the thread pool for processor execution.

    Returns:
        Thread pool executor
    """
    global _thread_pool
    if _thread_pool is None:
        _thread_pool = ThreadPoolExecutor(max_workers=4, thread_name_prefix="fastapi_processor_")
    return _thread_pool


def shutdown_thread_pool() -> None:
    """Shutdown the thread pool (called on app shutdown)."""
    global _thread_pool
    if _thread_pool:
        _thread_pool.shutdown(wait=True)
        _thread_pool = None


# Pydantic models for API requests (FastAPI will validate these)


class BuildRequestModel(BaseModel):
    """Build request model for FastAPI."""

    project_dir: str = Field(..., description="Absolute path to project directory")
    environment: str = Field(..., description="Build environment name")
    clean_build: bool = Field(False, description="Whether to perform clean build")
    verbose: bool = Field(False, description="Enable verbose build output")
    caller_pid: int = Field(..., description="Process ID of requesting client")
    caller_cwd: str = Field(..., description="Working directory of requesting client")
    jobs: int | None = Field(None, description="Number of parallel compilation jobs (None = CPU count)")
    request_id: str | None = Field(None, description="Unique identifier for this request")

    def to_build_request(self) -> BuildRequest:
        """Convert to BuildRequest message."""
        return BuildRequest(
            project_dir=self.project_dir,
            environment=self.environment,
            clean_build=self.clean_build,
            verbose=self.verbose,
            caller_pid=self.caller_pid,
            caller_cwd=self.caller_cwd,
            jobs=self.jobs,
            request_id=self.request_id or f"build_{int(asyncio.get_event_loop().time() * 1000)}",
        )


class DeployRequestModel(BaseModel):
    """Deploy request model for FastAPI."""

    project_dir: str = Field(..., description="Absolute path to project directory")
    environment: str = Field(..., description="Build environment name")
    port: str | None = Field(None, description="Serial port for deployment (optional, auto-detect if None)")
    clean_build: bool = Field(False, description="Whether to perform clean build")
    monitor_after: bool = Field(False, description="Whether to start monitor after deploy")
    monitor_timeout: float | None = Field(None, description="Timeout for monitor in seconds")
    monitor_halt_on_error: str | None = Field(None, description="Pattern to halt on error")
    monitor_halt_on_success: str | None = Field(None, description="Pattern to halt on success")
    monitor_expect: str | None = Field(None, description="Expected pattern to check at timeout/success")
    monitor_show_timestamp: bool = Field(False, description="Whether to prefix monitor output lines with elapsed time")
    caller_pid: int = Field(..., description="Process ID of requesting client")
    caller_cwd: str = Field(..., description="Working directory of requesting client")
    skip_build: bool = Field(False, description="Whether to skip the build phase (upload-only mode)")
    request_id: str | None = Field(None, description="Unique identifier for this request")

    def to_deploy_request(self) -> DeployRequest:
        """Convert to DeployRequest message."""
        return DeployRequest(
            project_dir=self.project_dir,
            environment=self.environment,
            port=self.port,
            clean_build=self.clean_build,
            monitor_after=self.monitor_after,
            monitor_timeout=self.monitor_timeout,
            monitor_halt_on_error=self.monitor_halt_on_error,
            monitor_halt_on_success=self.monitor_halt_on_success,
            monitor_expect=self.monitor_expect,
            monitor_show_timestamp=self.monitor_show_timestamp,
            caller_pid=self.caller_pid,
            caller_cwd=self.caller_cwd,
            skip_build=self.skip_build,
            request_id=self.request_id or f"deploy_{int(asyncio.get_event_loop().time() * 1000)}",
        )


class MonitorRequestModel(BaseModel):
    """Monitor request model for FastAPI."""

    project_dir: str = Field(..., description="Absolute path to project directory")
    environment: str = Field(..., description="Build environment name")
    port: str | None = Field(None, description="Serial port for monitoring (optional, auto-detect if None)")
    baud_rate: int | None = Field(None, description="Serial baud rate (optional, use config default if None)")
    halt_on_error: str | None = Field(None, description="Pattern to halt on (error detection)")
    halt_on_success: str | None = Field(None, description="Pattern to halt on (success detection)")
    expect: str | None = Field(None, description="Expected pattern to check at timeout/success")
    timeout: float | None = Field(None, description="Maximum monitoring time in seconds")
    caller_pid: int = Field(..., description="Process ID of requesting client")
    caller_cwd: str = Field(..., description="Working directory of requesting client")
    show_timestamp: bool = Field(False, description="Whether to prefix output lines with elapsed time")
    request_id: str | None = Field(None, description="Unique identifier for this request")

    def to_monitor_request(self) -> MonitorRequest:
        """Convert to MonitorRequest message."""
        return MonitorRequest(
            project_dir=self.project_dir,
            environment=self.environment,
            port=self.port,
            baud_rate=self.baud_rate,
            halt_on_error=self.halt_on_error,
            halt_on_success=self.halt_on_success,
            expect=self.expect,
            timeout=self.timeout,
            caller_pid=self.caller_pid,
            caller_cwd=self.caller_cwd,
            show_timestamp=self.show_timestamp,
            request_id=self.request_id or f"monitor_{int(asyncio.get_event_loop().time() * 1000)}",
        )


class InstallDepsRequestModel(BaseModel):
    """Install dependencies request model for FastAPI."""

    project_dir: str = Field(..., description="Absolute path to project directory")
    environment: str = Field(..., description="Build environment name")
    caller_pid: int = Field(..., description="Process ID of requesting client")
    caller_cwd: str = Field(..., description="Working directory of requesting client")
    request_id: str | None = Field(None, description="Unique identifier for this request")

    def to_install_deps_request(self) -> InstallDependenciesRequest:
        """Convert to InstallDependenciesRequest message."""
        return InstallDependenciesRequest(
            project_dir=self.project_dir,
            environment=self.environment,
            verbose=False,
            caller_pid=self.caller_pid,
            caller_cwd=self.caller_cwd,
            request_id=self.request_id or f"install_deps_{int(asyncio.get_event_loop().time() * 1000)}",
        )


class OperationResponse(BaseModel):
    """Response for operation requests."""

    success: bool = Field(..., description="Whether the operation succeeded")
    request_id: str = Field(..., description="Request ID that was processed")
    message: str = Field(..., description="Human-readable status message")
    exit_code: int = Field(0, description="Exit code (0 for success, non-zero for failure)")
    output_file: str | None = Field(None, description="Path to output file (for build operations)")


def create_operations_router(get_daemon_context_func: Any) -> APIRouter:
    """Create the operations router.

    Args:
        get_daemon_context_func: Dependency injection function for DaemonContext

    Returns:
        Configured APIRouter
    """
    router = APIRouter(prefix="/api", tags=["Operations"])

    @router.post("/build", response_model=OperationResponse)
    async def build_endpoint(  # type: ignore[reportUnusedFunction]
        request: BuildRequestModel,
        context: DaemonContext = Depends(get_daemon_context_func),
    ) -> OperationResponse:
        """Execute a build operation.

        This endpoint compiles the project using the specified environment.
        The build runs in a background thread to avoid blocking the event loop.

        Returns:
            Build result with success status and output file path
        """
        logging.info(f"Received build request: env={request.environment}, project={request.project_dir}")

        # Convert to BuildRequest message
        build_request = request.to_build_request()

        # Run processor in thread pool (processors are synchronous)
        from fbuild.daemon.processors.build_processor import BuildRequestProcessor

        processor = BuildRequestProcessor()

        loop = asyncio.get_event_loop()
        success = await loop.run_in_executor(
            get_thread_pool(),
            processor.process_request,
            build_request,
            context,
        )

        # Get output file path
        from pathlib import Path

        output_file = str(Path(request.project_dir) / ".fbuild" / "build_output.txt")

        return OperationResponse(
            success=success,
            request_id=build_request.request_id,
            message="Build successful" if success else processor.get_failure_message(build_request),
            exit_code=0 if success else 1,
            output_file=output_file if success else None,
        )

    @router.post("/deploy", response_model=OperationResponse)
    async def deploy_endpoint(  # type: ignore[reportUnusedFunction]
        request: DeployRequestModel,
        context: DaemonContext = Depends(get_daemon_context_func),
    ) -> OperationResponse:
        """Execute a deploy operation.

        This endpoint builds (if needed) and deploys firmware to a device.
        Optionally starts monitoring after deployment.

        Returns:
            Deploy result with success status
        """
        logging.info(f"Received deploy request: env={request.environment}, project={request.project_dir}")

        # Convert to DeployRequest message
        deploy_request = request.to_deploy_request()

        # Run processor in thread pool
        from fbuild.daemon.processors.deploy_processor import DeployRequestProcessor

        processor = DeployRequestProcessor()

        loop = asyncio.get_event_loop()
        success = await loop.run_in_executor(
            get_thread_pool(),
            processor.process_request,
            deploy_request,
            context,
        )

        return OperationResponse(
            success=success,
            request_id=deploy_request.request_id,
            message="Deploy successful" if success else processor.get_failure_message(deploy_request),
            exit_code=0 if success else 1,
            output_file=None,
        )

    @router.post("/monitor", response_model=OperationResponse)
    async def monitor_endpoint(  # type: ignore[reportUnusedFunction]
        request: MonitorRequestModel,
        context: DaemonContext = Depends(get_daemon_context_func),
    ) -> OperationResponse:
        """Execute a monitor operation.

        This endpoint monitors serial output from a device.
        Can halt on specific patterns or timeout.

        Returns:
            Monitor result with success status
        """
        logging.info(f"Received monitor request: env={request.environment}, project={request.project_dir}")

        # Convert to MonitorRequest message
        monitor_request = request.to_monitor_request()

        # Run processor in thread pool
        from fbuild.daemon.processors.monitor_processor import MonitorRequestProcessor

        processor = MonitorRequestProcessor()

        loop = asyncio.get_event_loop()
        success = await loop.run_in_executor(
            get_thread_pool(),
            processor.process_request,
            monitor_request,
            context,
        )

        return OperationResponse(
            success=success,
            request_id=monitor_request.request_id,
            message="Monitor completed" if success else processor.get_failure_message(monitor_request),
            exit_code=0 if success else 1,
            output_file=None,
        )

    @router.post("/install-deps", response_model=OperationResponse)
    async def install_deps_endpoint(  # type: ignore[reportUnusedFunction]
        request: InstallDepsRequestModel,
        context: DaemonContext = Depends(get_daemon_context_func),
    ) -> OperationResponse:
        """Install project dependencies.

        This endpoint downloads and installs all required dependencies
        for the specified environment (toolchains, frameworks, libraries).

        Returns:
            Installation result with success status
        """
        logging.info(f"Received install-deps request: env={request.environment}, project={request.project_dir}")

        # Convert to InstallDependenciesRequest message
        install_request = request.to_install_deps_request()

        # Run processor in thread pool
        from fbuild.daemon.processors.install_deps_processor import (
            InstallDependenciesProcessor,
        )

        processor = InstallDependenciesProcessor()

        loop = asyncio.get_event_loop()
        success = await loop.run_in_executor(
            get_thread_pool(),
            processor.process_request,  # type: ignore[arg-type]
            install_request,
            context,
        )

        return OperationResponse(
            success=success,
            request_id=install_request.request_id,
            message="Dependencies installed" if success else processor.get_failure_message(install_request),
            exit_code=0 if success else 1,
            output_file=None,
        )

    return router
