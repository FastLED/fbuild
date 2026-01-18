"""
fbuild Daemon - Concurrent Deploy and Monitor Management

This package provides a singleton daemon for managing concurrent deploy and monitor
operations with proper locking and process tree tracking.
"""

from fbuild.daemon.client import (
    ensure_daemon_running,
    get_daemon_status,
    request_build,
    request_deploy,
    request_install_dependencies,
    request_monitor,
    stop_daemon,
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
    "DaemonState",
    "DaemonStatus",
    "DeployRequest",
    "InstallDependenciesRequest",
    "MonitorRequest",
    "OperationType",
    "ensure_daemon_running",
    "get_daemon_status",
    "request_build",
    "request_deploy",
    "request_install_dependencies",
    "request_monitor",
    "stop_daemon",
]
