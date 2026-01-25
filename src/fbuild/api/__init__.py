"""fbuild Python API - Programmatic access to daemon functionality.

This module provides public Python APIs for interacting with the fbuild daemon
without using the CLI. These APIs enable external scripts (like CI validation)
to route operations through the daemon, eliminating OS-level port conflicts.

Available APIs:
- SerialMonitor: Context manager for daemon-routed serial I/O
  Enables multiple clients to monitor serial output concurrently and allows
  deploy operations to preempt gracefully.

Example:
    >>> from fbuild.api import SerialMonitor
    >>>
    >>> with SerialMonitor(port='COM13', baud_rate=115200) as mon:
    ...     for line in mon.read_lines(timeout=30.0):
    ...         print(line)
    ...         if 'READY' in line:
    ...             break
"""

from fbuild.api.serial_monitor import MonitorHook, SerialMonitor

__all__ = ["SerialMonitor", "MonitorHook"]
