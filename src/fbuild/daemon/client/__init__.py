"""
fbuild Daemon Client

Client interface for interacting with the fbuild daemon via HTTP/WebSocket.
All operations now use FastAPI HTTP endpoints instead of file-based IPC.
"""

# Device management (HTTP-based)
from .devices_http import (
    acquire_device_lease_http,
    get_device_status_http,
    list_devices_http,
    preempt_device_http,
    release_device_lease_http,
)

# Lifecycle management
from .lifecycle import (
    ensure_daemon_running,
    is_daemon_running,
    stop_daemon,
)

# Lock management (HTTP-based)
from .locks_http import (
    clear_stale_locks_http,
    display_lock_status_http,
    get_lock_status_http,
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

# Request handling (HTTP-based)
from .requests_http import (
    request_build_http,
    request_deploy_http,
    request_install_dependencies_http,
    request_monitor_http,
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
    "ensure_daemon_running",
    "is_daemon_running",
    "stop_daemon",
    # Status
    "SPINNER_CHARS",
    "display_daemon_stats_compact",
    "display_spinner_progress",
    "display_status",
    "get_daemon_status",
    "read_status_file",
    # Requests (HTTP-based)
    "request_build_http",
    "request_deploy_http",
    "request_install_dependencies_http",
    "request_monitor_http",
    # Locks (HTTP-based)
    "display_lock_status_http",
    "get_lock_status_http",
    "clear_stale_locks_http",
    # Devices (HTTP-based)
    "acquire_device_lease_http",
    "get_device_status_http",
    "list_devices_http",
    "preempt_device_http",
    "release_device_lease_http",
    # Management
    "display_daemon_list",
    "force_kill_daemon",
    "get_daemon_log_path",
    "graceful_kill_daemon",
    "kill_all_daemons",
    "list_all_daemons",
    "tail_daemon_logs",
]
