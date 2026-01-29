"""
Daemon paths configuration.

Centralized path definitions for daemon files. Supports development mode
to isolate daemon instances when running from repo.
"""

import os
from pathlib import Path

# Daemon configuration
DAEMON_NAME = "fbuild_daemon"  # Exported for backward compatibility

# Check for development mode (when running from repo)
if os.environ.get("FBUILD_DEV_MODE") == "1":
    # Use project-local daemon directory for development
    DAEMON_DIR = Path.cwd() / ".fbuild" / "daemon_dev"
else:
    # Use home directory for production
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
