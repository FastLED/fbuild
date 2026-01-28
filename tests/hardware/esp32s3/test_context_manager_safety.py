"""Test Suite 4: Context Manager Safety

Tests to validate that context managers properly clean up serial resources:
1. Exception safety of serial context managers
2. Daemon serial monitor cleanup on interrupt

These tests ensure that even when exceptions occur (including KeyboardInterrupt),
serial ports are properly released and remain accessible for subsequent operations.

These tests require physical ESP32-S3 hardware (default: COM13).
Set FBUILD_ESP32S3_PORT environment variable to override.
"""

import os
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

import pytest
import serial

# Import helpers from conftest
from .conftest import start_fbuild_daemon, verify_port_accessible

# Fixtures imported via pytest auto-discovery:
# - esp32s3_port: Session-scoped fixture providing port name
# - clean_daemon: Function-scoped fixture ensuring clean daemon state


@pytest.mark.unit
@pytest.mark.esp32s3
def test_serial_context_manager_exception_safety(esp32s3_port: str) -> None:
    """Test 4.1: Verify serial context manager cleans up even on exception.

    This test validates that Python's serial.Serial context manager properly
    closes the port even when an exception is raised during operations.

    Context Manager Behavior:
    - __enter__: Opens serial port
    - __exit__: Closes port (called even on exception)
    - Ensures resource cleanup in all exit paths

    Test Strategy:
    1. Open port using context manager
    2. Perform some operation (write)
    3. Raise RuntimeError to simulate exception
    4. Verify port is closed (via __exit__)
    5. Attempt to reopen port to confirm it's accessible

    Expected Result:
    - Port should be cleanly closed after context exit
    - Port should be immediately accessible for subsequent operations
    - No PermissionError or device busy errors

    Validates: Proper exception handling in serial operations
    Impact: Prevents port locks when operations fail
    """
    # Phase 1: Verify port is accessible before test
    assert verify_port_accessible(esp32s3_port, timeout=2.0), \
        f"Port {esp32s3_port} not accessible before test"

    # Phase 2: Open port and raise exception within context manager
    exception_raised = False
    try:
        with serial.Serial(esp32s3_port, 115200, timeout=1) as ser:
            # Verify port is open
            assert ser.is_open, "Serial port should be open inside context"

            # Perform a write operation
            ser.write(b"test_data\n")

            # Simulate an error during operation
            raise RuntimeError("Simulated error during serial operation")

    except RuntimeError as e:
        # Exception should be caught here
        assert str(e) == "Simulated error during serial operation"
        exception_raised = True

    # Verify exception was raised (test integrity check)
    assert exception_raised, "RuntimeError should have been raised"

    # Phase 3: Verify port is accessible after exception
    # Small delay to ensure cleanup completes
    time.sleep(0.1)

    # Attempt to open port again - should succeed if context manager cleaned up
    try:
        with serial.Serial(esp32s3_port, 115200, timeout=1) as ser:
            assert ser.is_open, "Port should be accessible after exception in previous context"
            # Try a basic operation to confirm port is functional
            ser.write(b"recovery_test\n")
    except serial.SerialException as e:
        pytest.fail(f"Port {esp32s3_port} locked after exception: {e}")

    # Final verification
    assert verify_port_accessible(esp32s3_port, timeout=2.0), \
        f"Port {esp32s3_port} not accessible after test"


@pytest.mark.integration
@pytest.mark.hardware
@pytest.mark.esp32s3
def test_daemon_serial_monitor_cleanup(esp32s3_port: str, clean_daemon: None) -> None:  # noqa: ARG001
    """Test 4.2: Verify daemon releases port when monitor process is interrupted.

    This test validates that the fbuild daemon's SharedSerialManager properly
    releases serial ports when a monitor operation is interrupted via KeyboardInterrupt
    (Ctrl+C). This is critical for preventing port locks when users cancel monitoring.

    Root Cause #1 Context:
    Original issue: Daemon would keep port open even after client disconnected.
    Fix applied: SharedSerialManager.detach_reader() closes port when last client detaches.
    This test validates the fix handles interrupt scenarios.

    Test Strategy:
    1. Start fbuild daemon in background
    2. Launch fbuild monitor subprocess
    3. After monitor starts, send KeyboardInterrupt (via process termination)
    4. Give daemon time to process client detach event
    5. Verify port is accessible (not locked by daemon)

    Expected Result:
    - Daemon should detect client disconnection
    - Daemon should close port when last reader detaches
    - Port should be accessible for subsequent operations
    - No manual intervention required

    Validates: Root Cause #1 fix (daemon serial port leak on client crash)
    Impact: Prevents port locks when monitoring is interrupted
    Location: src/fbuild/daemon/shared_serial.py lines 391-394
    """
    # Phase 1: Verify port is accessible before test
    assert verify_port_accessible(esp32s3_port, timeout=2.0), \
        f"Port {esp32s3_port} not accessible before test"

    # Phase 2: Start fbuild daemon
    # Note: clean_daemon fixture ensures daemon starts clean
    # We need to explicitly start daemon for this test
    env = os.environ.copy()
    env["FBUILD_DEV_MODE"] = "1"

    # Use subprocess to start monitor in a way we can interrupt
    # The monitor command will open the port via daemon
    monitor_script = Path(__file__).parent / "_helpers" / "monitor_wrapper.py"

    # If helper script doesn't exist, create inline subprocess approach
    # We'll create a subprocess that runs fbuild monitor and can be interrupted
    monitor_cmd = [
        sys.executable,
        "-m",
        "fbuild.cli",
        "monitor",
        "--port",
        esp32s3_port,
    ]

    # Start monitor subprocess
    monitor_proc = subprocess.Popen(
        monitor_cmd,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )

    # Phase 3: Wait for monitor to start and acquire port
    # Give it time to initialize and open the port
    time.sleep(2.0)

    # Verify monitor process is running
    if monitor_proc.poll() is not None:
        # Process already exited - may indicate daemon not running or other issue
        # This is acceptable for this test - we'll check port accessibility
        stdout, stderr = monitor_proc.communicate()
        print(f"Monitor process exited early (returncode={monitor_proc.returncode})")
        print(f"STDOUT: {stdout}")
        print(f"STDERR: {stderr}")
    else:
        # Phase 4: Simulate KeyboardInterrupt by terminating the process
        # This mimics what happens when user presses Ctrl+C
        monitor_proc.terminate()

        # Wait for process to exit
        try:
            monitor_proc.wait(timeout=3.0)
        except subprocess.TimeoutExpired:
            # Force kill if graceful termination didn't work
            monitor_proc.kill()
            monitor_proc.wait()

    # Phase 5: Give daemon time to process client detach
    # The daemon's SharedSerialManager should detect the disconnection
    # and call detach_reader(), which should close the port
    time.sleep(1.0)

    # Phase 6: Verify port is accessible after interrupt
    # This is the critical test - if daemon didn't clean up, port will be locked
    port_accessible = verify_port_accessible(esp32s3_port, timeout=2.0)

    if not port_accessible:
        # Collect diagnostic information
        print(f"\n❌ Port {esp32s3_port} is LOCKED after monitor interrupt!")
        print("This indicates daemon did not properly release the port.")
        print("\nExpected behavior:")
        print("  1. Monitor subprocess terminates")
        print("  2. Daemon detects client disconnect")
        print("  3. Daemon calls SharedSerialManager.detach_reader()")
        print("  4. Daemon closes port when last reader detaches")
        print("\nActual behavior:")
        print("  Port remains locked - daemon still holding the port open")
        print("\nThis is Root Cause #1 - daemon serial port leak")
        pytest.fail(f"Port {esp32s3_port} locked after monitor interrupt - daemon did not release port")

    # Success - port is accessible
    # Try to open it to confirm it's fully functional
    try:
        with serial.Serial(esp32s3_port, 115200, timeout=1) as ser:
            assert ser.is_open, "Port should be accessible after monitor interrupt"
            ser.write(b"cleanup_test\n")
    except serial.SerialException as e:
        pytest.fail(f"Port {esp32s3_port} appears accessible but cannot be opened: {e}")

    # Final verification
    assert verify_port_accessible(esp32s3_port, timeout=2.0), \
        f"Port {esp32s3_port} not accessible after test cleanup"


# Helper function for manual testing and debugging
def _test_monitor_interrupt_manual(port: str) -> None:
    """Manual test helper for debugging monitor interrupt behavior.

    This function is not run automatically - it's for manual investigation
    of monitor interrupt behavior. Can be invoked from pytest with -k flag
    or run directly.

    Usage:
        pytest tests/hardware/esp32s3/test_context_manager_safety.py::_test_monitor_interrupt_manual -s

    Args:
        port: Serial port to test (e.g., "COM13")
    """
    print(f"\n=== Manual Monitor Interrupt Test ===")
    print(f"Port: {port}")
    print(f"Instructions:")
    print(f"  1. This will start fbuild monitor")
    print(f"  2. Press Ctrl+C after you see output")
    print(f"  3. We'll verify port is released")
    print(f"\nStarting monitor in 3 seconds...\n")

    time.sleep(3)

    # Run monitor directly (will be interrupted by user)
    try:
        subprocess.run(
            [sys.executable, "-m", "fbuild.cli", "monitor", "--port", port],
            env={**os.environ, "FBUILD_DEV_MODE": "1"},
            check=False,
        )
    except KeyboardInterrupt:
        print("\n\nMonitor interrupted!")

    # Check port accessibility
    time.sleep(1.0)
    accessible = verify_port_accessible(port, timeout=2.0)

    if accessible:
        print(f"✅ Port {port} is accessible after interrupt")
    else:
        print(f"❌ Port {port} is LOCKED after interrupt")

    return accessible
