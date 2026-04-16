"""Integration test: reset a real ESP32-S3 via the fbuild daemon.

Requires:
  - fbuild daemon running
  - ESP32-S3 device on COM13
  - Device flashed with AutoResearch firmware

Skip conditions checked at import time.
"""

from __future__ import annotations

import time

import pytest
import serial as pyserial


def _port_exists(port: str) -> bool:
    """Check if a serial port exists and can be opened."""
    try:
        s = pyserial.Serial(port, 115200, timeout=0.1)
        s.close()
        return True
    except (OSError, pyserial.SerialException):
        return False


def _daemon_running() -> bool:
    """Check if the fbuild daemon is reachable."""
    try:
        import urllib.request

        urllib.request.urlopen("http://127.0.0.1:8765/health", timeout=2)
        return True
    except Exception:
        return False


@pytest.mark.skipif(not _port_exists("COM13"), reason="COM13 not available")
@pytest.mark.skipif(not _daemon_running(), reason="fbuild daemon not running")
def test_reset_esp32s3_and_read_output() -> None:
    """After reset_device, a fresh monitor should capture device output."""
    from fbuild._native import SerialMonitor

    # Reset first (no monitor attached — avoids preemption race)
    mon = SerialMonitor(port="COM13", baud_rate=115200)
    result = mon.reset_device(board="esp32s3")
    assert result is True, "reset_device should return True on success"

    # Wait for reboot
    time.sleep(3.0)

    # Now attach a fresh monitor and read output
    mon2 = SerialMonitor(port="COM13", baud_rate=115200)
    mon2.__enter__()
    try:
        lines = mon2.read_lines(timeout=5.0)
        assert len(lines) > 0, (
            "Device should produce serial output after reset. "
            "Got 0 lines — device may not have rebooted."
        )
    finally:
        mon2.__exit__(None, None, None)


@pytest.mark.skipif(not _port_exists("COM13"), reason="COM13 not available")
@pytest.mark.skipif(not _daemon_running(), reason="fbuild daemon not running")
def test_reset_device_without_enter() -> None:
    """reset_device should work without calling __enter__ first."""
    from fbuild._native import SerialMonitor

    mon = SerialMonitor(port="COM13", baud_rate=115200)
    # Should NOT require __enter__ — reset goes through HTTP, not WebSocket
    result = mon.reset_device(board="esp32s3")
    assert isinstance(result, bool), "reset_device must return a bool"
    assert result is True, "reset_device should succeed for a connected ESP32-S3"
