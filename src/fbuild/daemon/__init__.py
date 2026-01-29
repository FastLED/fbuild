"""
fbuild Daemon - Concurrent Deploy and Monitor Management

This package provides a singleton daemon for managing concurrent deploy and monitor
operations with proper locking and process tree tracking.

All operations now use HTTP/WebSocket communication via FastAPI.
"""

from fbuild.daemon.client import (
    ensure_daemon_running,
    get_daemon_status,
    request_build_http,
    request_deploy_http,
    request_install_dependencies_http,
    request_monitor_http,
    stop_daemon,
)
from fbuild.daemon.compilation_queue import CompilationJobQueue
from fbuild.daemon.connection import DaemonConnection, connect_daemon
from fbuild.daemon.connection_registry import (
    ConnectionRegistry,
    ConnectionState,
    PlatformSlot,
)
from fbuild.daemon.messages import (
    BuildRequest,
    DaemonState,
    DaemonStatus,
    DeployRequest,
    InstallDependenciesRequest,
    MonitorRequest,
    OperationType,
)

__all__ = [
    "BuildRequest",
    "CompilationJobQueue",
    "ConnectionRegistry",
    "ConnectionState",
    "DaemonConnection",
    "DaemonState",
    "DaemonStatus",
    "DeployRequest",
    "InstallDependenciesRequest",
    "MonitorRequest",
    "OperationType",
    "PlatformSlot",
    "connect_daemon",
    "ensure_daemon_running",
    "get_daemon_status",
    "request_build_http",
    "request_deploy_http",
    "request_install_dependencies_http",
    "request_monitor_http",
    "stop_daemon",
]
