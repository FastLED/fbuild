"""
Daemon paths configuration.

Centralized path definitions for daemon files. Supports development mode
to isolate daemon instances.

Modes:
- Production (default): ~/.fbuild/daemon/
- Development (FBUILD_DEV_MODE=1): ~/.fbuild/daemon_dev/ (isolated from prod)
"""

import os
from pathlib import Path

# Daemon configuration
DAEMON_NAME = "fbuild_daemon"  # Exported for backward compatibility


def is_dev_mode() -> bool:
    """Check if development mode is enabled."""
    return os.environ.get("FBUILD_DEV_MODE") == "1"


# Determine daemon directory based on mode
if is_dev_mode():
    # Development mode: use ~/.fbuild/daemon_dev/ (isolated from prod daemon)
    DAEMON_DIR = Path.home() / ".fbuild" / "daemon_dev"
else:
    # Production: use home directory
    DAEMON_DIR = Path.home() / ".fbuild" / "daemon"

# Core daemon files
PID_FILE = DAEMON_DIR / f"{DAEMON_NAME}.pid"
LOCK_FILE = DAEMON_DIR / f"{DAEMON_NAME}.lock"
STATUS_FILE = DAEMON_DIR / "daemon_status.json"
LOG_FILE = DAEMON_DIR / "daemon.log"

# Request/response files
BUILD_REQUEST_FILE = DAEMON_DIR / "build_request.json"
DEPLOY_REQUEST_FILE = DAEMON_DIR / "deploy_request.json"
MONITOR_REQUEST_FILE = DAEMON_DIR / "monitor_request.json"
INSTALL_DEPS_REQUEST_FILE = DAEMON_DIR / "install_deps_request.json"

# Device management request/response files
DEVICE_LIST_REQUEST_FILE = DAEMON_DIR / "device_list_request.json"
DEVICE_LIST_RESPONSE_FILE = DAEMON_DIR / "device_list_response.json"
DEVICE_STATUS_REQUEST_FILE = DAEMON_DIR / "device_status_request.json"
DEVICE_STATUS_RESPONSE_FILE = DAEMON_DIR / "device_status_response.json"
DEVICE_LEASE_REQUEST_FILE = DAEMON_DIR / "device_lease_request.json"
DEVICE_LEASE_RESPONSE_FILE = DAEMON_DIR / "device_lease_response.json"
DEVICE_RELEASE_REQUEST_FILE = DAEMON_DIR / "device_release_request.json"
DEVICE_RELEASE_RESPONSE_FILE = DAEMON_DIR / "device_release_response.json"

# Other daemon files
PROCESS_REGISTRY_FILE = DAEMON_DIR / "process_registry.json"
FILE_CACHE_FILE = DAEMON_DIR / "file_cache.json"

# Serial Monitor API files (used by fbuild.api.SerialMonitor)
SERIAL_MONITOR_ATTACH_REQUEST_FILE = DAEMON_DIR / "serial_monitor_attach_request.json"
SERIAL_MONITOR_DETACH_REQUEST_FILE = DAEMON_DIR / "serial_monitor_detach_request.json"
SERIAL_MONITOR_POLL_REQUEST_FILE = DAEMON_DIR / "serial_monitor_poll_request.json"
SERIAL_MONITOR_RESPONSE_FILE = DAEMON_DIR / "serial_monitor_response.json"
