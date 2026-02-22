"""fbuild Python API - Programmatic access to daemon functionality.

This module provides public Python APIs for interacting with the fbuild daemon
without using the CLI. These APIs enable external scripts (like CI validation)
to route operations through the daemon, eliminating OS-level port conflicts.

Available APIs:
- SerialMonitor: Sync context manager for daemon-routed serial I/O
- AsyncSerialMonitor: Async context manager for daemon-routed serial I/O
  Native async/await API, safe to use within running event loops.

Example (sync):
    >>> from fbuild.api import SerialMonitor
    >>>
    >>> with SerialMonitor(port='COM13', baud_rate=115200) as mon:
    ...     for line in mon.read_lines(timeout=30.0):
    ...         print(line)
    ...         if 'READY' in line:
    ...             break

Example (async):
    >>> import asyncio
    >>> from fbuild.api import AsyncSerialMonitor
    >>>
    >>> async def main():
    ...     async with AsyncSerialMonitor(port='COM13', baud_rate=115200) as mon:
    ...         async for line in mon.read_lines(timeout=30.0):
    ...             print(line)
    ...             if 'READY' in line:
    ...                 break
"""

from fbuild.api.async_serial_monitor import AsyncMonitorHook, AsyncSerialMonitor
from fbuild.api.serial_monitor import MonitorHook, SerialMonitor

__all__ = ["AsyncMonitorHook", "AsyncSerialMonitor", "MonitorHook", "SerialMonitor"]
