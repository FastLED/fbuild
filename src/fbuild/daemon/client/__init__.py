"""
fbuild Daemon Client

Client interface for interacting with the fbuild daemon.
Handles lifecycle, requests, status monitoring, locks, devices, and process management.
"""

# Device management
from .devices import (
    acquire_device_lease,
    get_device_status,
    list_devices,
    preempt_device,
    release_device_lease,
)

# Lifecycle management
from .lifecycle import (
    BUILD_REQUEST_FILE,
    DAEMON_DIR,
    DAEMON_NAME,
    DEPLOY_REQUEST_FILE,
    INSTALL_DEPS_REQUEST_FILE,
    MONITOR_REQUEST_FILE,
    PID_FILE,
    SERIAL_MONITOR_ATTACH_REQUEST_FILE,
    SERIAL_MONITOR_DETACH_REQUEST_FILE,
    SERIAL_MONITOR_POLL_REQUEST_FILE,
    SERIAL_MONITOR_RESPONSE_FILE,
    STATUS_FILE,
    ensure_daemon_running,
    is_daemon_running,
    start_daemon,
    stop_daemon,
)

# Lock management
from .locks import (
    display_lock_status,
    get_lock_status,
    request_clear_stale_locks,
)

# Process management
from .management import (
    display_daemon_list,
    force_kill_daemon,
    get_daemon_log_path,
    graceful_kill_daemon,
    kill_all_daemons,
    list_all_daemons,
    tail_daemon_logs,
)

# Request handling
from .requests import (
    BaseRequestHandler,
    BuildRequestHandler,
    DeployRequestHandler,
    InstallDependenciesRequestHandler,
    MonitorRequestHandler,
    request_build,
    request_deploy,
    request_install_dependencies,
    request_monitor,
    write_request_file,
)

# Status monitoring
from .status import (
    SPINNER_CHARS,
    display_daemon_stats_compact,
    display_spinner_progress,
    display_status,
    get_daemon_status,
    read_status_file,
)

__all__ = [
    # Lifecycle
    "DAEMON_DIR",
    "DAEMON_NAME",
    "PID_FILE",
    "STATUS_FILE",
    "BUILD_REQUEST_FILE",
    "DEPLOY_REQUEST_FILE",
    "MONITOR_REQUEST_FILE",
    "INSTALL_DEPS_REQUEST_FILE",
    "SERIAL_MONITOR_ATTACH_REQUEST_FILE",
    "SERIAL_MONITOR_DETACH_REQUEST_FILE",
    "SERIAL_MONITOR_POLL_REQUEST_FILE",
    "SERIAL_MONITOR_RESPONSE_FILE",
    "ensure_daemon_running",
    "is_daemon_running",
    "start_daemon",
    "stop_daemon",
    # Status
    "SPINNER_CHARS",
    "display_daemon_stats_compact",
    "display_spinner_progress",
    "display_status",
    "get_daemon_status",
    "read_status_file",
    # Requests
    "BaseRequestHandler",
    "BuildRequestHandler",
    "DeployRequestHandler",
    "InstallDependenciesRequestHandler",
    "MonitorRequestHandler",
    "request_build",
    "request_deploy",
    "request_install_dependencies",
    "request_monitor",
    "write_request_file",
    # Locks
    "display_lock_status",
    "get_lock_status",
    "request_clear_stale_locks",
    # Devices
    "acquire_device_lease",
    "get_device_status",
    "list_devices",
    "preempt_device",
    "release_device_lease",
    # Management
    "display_daemon_list",
    "force_kill_daemon",
    "get_daemon_log_path",
    "graceful_kill_daemon",
    "kill_all_daemons",
    "list_all_daemons",
    "tail_daemon_logs",
]
