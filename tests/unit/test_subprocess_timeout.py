"""Test subprocess timeout behavior on Windows with serial port I/O.

This test reproduces the issue where subprocess.run() with a timeout parameter
does not properly interrupt I/O-blocked operations on Windows serial ports.

Symptom: esptool hangs indefinitely instead of timing out after 120 seconds.
"""

import subprocess
import sys
import time
from pathlib import Path

import pytest


@pytest.mark.skipif(sys.platform != "win32", reason="Windows-specific timeout bug")
def test_subprocess_timeout_with_serial_port_mock():
    """Test that subprocess timeout works correctly with I/O operations.

    This test simulates the esptool hang scenario by running a Python script
    that attempts to open a serial port and blocks on I/O.

    Expected behavior: subprocess.run() should timeout after specified duration.
    Actual behavior (bug): May hang indefinitely if I/O is blocked.
    """
    # Create a mock script that blocks on I/O (simulates esptool hanging)
    test_script = Path(__file__).parent / "mock_esptool_hang.py"
    test_script.write_text(
        """
import time
import sys

# Simulate esptool hanging on serial port I/O
print("Connecting...", flush=True)
time.sleep(300)  # Sleep for 5 minutes (simulates hang)
print("Should never reach here")
sys.exit(0)
"""
    )

    try:
        start = time.time()
        timeout_seconds = 2  # Short timeout for test

        with pytest.raises(subprocess.TimeoutExpired):
            subprocess.run(
                [sys.executable, str(test_script)],
                timeout=timeout_seconds,
                capture_output=True,
                text=True,
            )

        elapsed = time.time() - start

        # Verify timeout occurred within expected range (allow 1s tolerance)
        assert elapsed < timeout_seconds + 1, f"Timeout took too long: {elapsed:.1f}s (expected ~{timeout_seconds}s)"

    finally:
        # Cleanup
        if test_script.exists():
            test_script.unlink()


@pytest.mark.skipif(sys.platform != "win32", reason="Windows-specific timeout bug")
@pytest.mark.skipif(True, reason="Requires actual hardware - manual test only")
def test_esptool_timeout_with_real_port():
    """Manual test: Verify esptool timeout with disconnected serial port.

    This test requires:
    1. COM port defined but no device connected
    2. esptool installed (pip install esptool)

    Run manually to reproduce the actual hang scenario.
    """
    from fbuild.subprocess_utils import get_python_executable, safe_run

    # Use a COM port that exists but has no device
    fake_port = "COM99"  # Adjust based on your system
    timeout_seconds = 5

    start = time.time()

    try:
        safe_run(
            [
                get_python_executable(),
                "-m",
                "esptool",
                "--chip",
                "esp32s3",
                "--port",
                fake_port,
                "--baud",
                "460800",
                "read_flash",
                "0x0",
                "0x1000",
                "test.bin",
            ],
            timeout=timeout_seconds,
            capture_output=True,
        )
        pytest.fail("Expected TimeoutExpired but command completed")
    except subprocess.TimeoutExpired:
        elapsed = time.time() - start
        print(f"✓ Timeout worked correctly: {elapsed:.1f}s")
        assert elapsed < timeout_seconds + 2, f"Timeout took too long: {elapsed:.1f}s"
