"""fbuild.api — Public Python API for serial monitoring.

Usage::

    from fbuild.api import SerialMonitor

    with SerialMonitor(port="COM13", baud_rate=115200) as mon:
        lines = mon.read_lines(timeout=30.0)
"""

from fbuild._native import AsyncSerialMonitor, SerialMonitor  # noqa: F401

__all__ = ["AsyncSerialMonitor", "SerialMonitor"]
