"""
Daemon paths configuration.

Centralized path definitions for daemon files. Supports development mode
to isolate daemon instances.

Modes:
- Production (default): ~/.fbuild/daemon/
- Development (FBUILD_DEV_MODE=1): ~/.fbuild/dev/daemon/ (isolated from prod)

IMPORTANT: All path constants (DAEMON_DIR, PID_FILE, etc.) are evaluated lazily
via __getattr__. This is necessary because cli.py sets FBUILD_DEV_MODE=1 AFTER
fbuild/__init__.py triggers the import of this module. Module-level constants
would capture the wrong mode.
"""

import os
from pathlib import Path

# Daemon configuration
DAEMON_NAME = "fbuild_daemon"  # Exported for backward compatibility


def is_dev_mode() -> bool:
    """Check if development mode is enabled."""
    return os.environ.get("FBUILD_DEV_MODE") == "1"


def get_daemon_dir() -> Path:
    """Get the daemon directory, respecting current FBUILD_DEV_MODE.

    This is evaluated on every call (not cached) because FBUILD_DEV_MODE
    may be set after this module is first imported.
    """
    if is_dev_mode():
        return Path.home() / ".fbuild" / "dev" / "daemon"
    return Path.home() / ".fbuild" / "daemon"


# Map of lazy attribute names to how they derive from DAEMON_DIR
_DERIVED_PATHS = {
    # Core daemon files
    "PID_FILE": lambda d: d / f"{DAEMON_NAME}.pid",
    "LOCK_FILE": lambda d: d / f"{DAEMON_NAME}.lock",
    "STATUS_FILE": lambda d: d / "daemon_status.json",
    "LOG_FILE": lambda d: d / "daemon.log",
    # Request/response files
    "BUILD_REQUEST_FILE": lambda d: d / "build_request.json",
    "DEPLOY_REQUEST_FILE": lambda d: d / "deploy_request.json",
    "MONITOR_REQUEST_FILE": lambda d: d / "monitor_request.json",
    "INSTALL_DEPS_REQUEST_FILE": lambda d: d / "install_deps_request.json",
    # Device management request/response files
    "DEVICE_LIST_REQUEST_FILE": lambda d: d / "device_list_request.json",
    "DEVICE_LIST_RESPONSE_FILE": lambda d: d / "device_list_response.json",
    "DEVICE_STATUS_REQUEST_FILE": lambda d: d / "device_status_request.json",
    "DEVICE_STATUS_RESPONSE_FILE": lambda d: d / "device_status_response.json",
    "DEVICE_LEASE_REQUEST_FILE": lambda d: d / "device_lease_request.json",
    "DEVICE_LEASE_RESPONSE_FILE": lambda d: d / "device_lease_response.json",
    "DEVICE_RELEASE_REQUEST_FILE": lambda d: d / "device_release_request.json",
    "DEVICE_RELEASE_RESPONSE_FILE": lambda d: d / "device_release_response.json",
    # Other daemon files
    "PROCESS_REGISTRY_FILE": lambda d: d / "process_registry.json",
    "FILE_CACHE_FILE": lambda d: d / "file_cache.json",
    # Serial Monitor API files (used by fbuild.api.SerialMonitor)
    "SERIAL_MONITOR_ATTACH_REQUEST_FILE": lambda d: d / "serial_monitor_attach_request.json",
    "SERIAL_MONITOR_DETACH_REQUEST_FILE": lambda d: d / "serial_monitor_detach_request.json",
    "SERIAL_MONITOR_POLL_REQUEST_FILE": lambda d: d / "serial_monitor_poll_request.json",
    "SERIAL_MONITOR_RESPONSE_FILE": lambda d: d / "serial_monitor_response.json",
}


def __getattr__(name: str) -> Path:
    """Lazy evaluation of path constants.

    Every access re-evaluates is_dev_mode() so that FBUILD_DEV_MODE changes
    (e.g. set by cli.py after initial import) are always respected.
    """
    if name == "DAEMON_DIR":
        return get_daemon_dir()
    if name in _DERIVED_PATHS:
        return _DERIVED_PATHS[name](get_daemon_dir())
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")


def compute_source_mtime() -> float:
    """Compute the max modification time of all fbuild source files.

    Scans all .py files under the fbuild package directory and returns the
    most recent mtime. Used to detect when source code has changed since
    the daemon was started (stale daemon detection).

    Returns:
        Max mtime as a float (Unix timestamp), or 0.0 if no files found.
    """
    import fbuild as _fbuild

    source_dir = Path(_fbuild.__file__).parent
    max_mtime = 0.0
    for py_file in source_dir.rglob("*.py"):
        try:
            mtime = py_file.stat().st_mtime
            if mtime > max_mtime:
                max_mtime = mtime
        except OSError:
            pass
    return max_mtime
