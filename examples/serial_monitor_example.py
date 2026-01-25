#!/usr/bin/env python3
"""Example: Using the SerialMonitor API for Validation Scripts

This script demonstrates how to migrate from direct pyserial usage to the
fbuild.api.SerialMonitor API. The SerialMonitor API routes all serial I/O
through the fbuild daemon, eliminating Windows driver-level port locks.

Benefits:
- Multiple clients can monitor the same port concurrently
- Deploy operations can preempt monitors gracefully (auto-reconnect)
- No PermissionError conflicts between validation and deployment

Before (direct pyserial - causes port locks):
    import serial
    ser = serial.Serial('COM13', 115200)
    data = ser.read(100)
    ser.close()

After (daemon-routed - no port locks):
    from fbuild.api import SerialMonitor
    with SerialMonitor('COM13', 115200) as mon:
        for line in mon.read_lines(timeout=30):
            print(line)
"""

import json
import sys
import time
from fbuild.api import SerialMonitor


def example_basic_monitoring():
    """Example 1: Basic serial monitoring with timeout."""
    print("Example 1: Basic monitoring")
    print("-" * 50)

    port = "COM13"  # Change to your port
    timeout = 30.0  # Monitor for 30 seconds

    with SerialMonitor(port=port, baud_rate=115200) as mon:
        print(f"Monitoring {port} for {timeout} seconds...")
        for line in mon.read_lines(timeout=timeout):
            print(f"[DEVICE] {line}")
            if "ERROR" in line:
                print("Error detected, aborting!")
                break


def example_with_hooks():
    """Example 2: Using hooks for pattern matching."""
    print("\nExample 2: Pattern matching with hooks")
    print("-" * 50)

    port = "COM13"
    found_ready = False

    def check_ready(line: str):
        """Hook that checks for 'READY' pattern."""
        nonlocal found_ready
        if "READY" in line:
            found_ready = True
            print(f"[HOOK] Device is ready! ({line})")

    def check_error(line: str):
        """Hook that aborts on error."""
        if "ERROR" in line or "FAIL" in line:
            raise RuntimeError(f"Device error detected: {line}")

    hooks = [check_ready, check_error]

    try:
        with SerialMonitor(port=port, baud_rate=115200, hooks=hooks) as mon:
            print(f"Monitoring {port} with hooks...")
            for line in mon.read_lines(timeout=60):
                print(f"[DEVICE] {line}")
                if found_ready:
                    print("Device ready, continuing...")
                    break
    except RuntimeError as e:
        print(f"Aborted: {e}")
        return False

    return True


def example_json_rpc():
    """Example 3: JSON-RPC communication with device."""
    print("\nExample 3: JSON-RPC communication")
    print("-" * 50)

    port = "COM13"

    with SerialMonitor(port=port, baud_rate=115200) as mon:
        # Send configuration request
        request = {
            "jsonrpc": "2.0",
            "method": "configure",
            "params": {"i2s_enabled": True, "tx_pin": 1, "rx_pin": 2},
            "id": 1,
        }

        print(f"Sending JSON-RPC request: {request}")
        response = mon.write_json_rpc(request, timeout=10.0)

        if response:
            print(f"Received response: {response}")
            if response.get("result") == "ok":
                print("Configuration successful!")
            else:
                print(f"Configuration failed: {response.get('error')}")
        else:
            print("No response received (timeout)")


def example_with_deploy_preemption():
    """Example 4: Monitoring that survives deploy preemption."""
    print("\nExample 4: Auto-reconnect during deploy")
    print("-" * 50)

    port = "COM13"

    # With auto_reconnect=True (default), monitoring pauses during deploy
    # and automatically resumes after deploy completes
    with SerialMonitor(port=port, baud_rate=115200, auto_reconnect=True, verbose=True) as mon:
        print(f"Monitoring {port} (will auto-reconnect if preempted by deploy)...")
        print("Try deploying firmware to the same port in another terminal!")
        print("The monitor will pause, wait for deploy, and resume automatically.")

        for line in mon.read_lines(timeout=120):
            print(f"[{time.time():.1f}] {line}")
            # Monitor will automatically handle deploy preemption
            # You'll see "[SerialMonitor] Deploy preempted..." logs if verbose=True


def example_wait_for_pattern():
    """Example 5: Wait for specific pattern with run_until."""
    print("\nExample 5: Wait for specific pattern")
    print("-" * 50)

    port = "COM13"

    with SerialMonitor(port=port, baud_rate=115200) as mon:
        print("Waiting for device to boot (looking for 'READY' pattern)...")

        # Wait until we see "READY" in output
        success = mon.run_until(
            condition=lambda: "READY" in mon.last_line,
            timeout=30.0,
        )

        if success:
            print(f"Device booted successfully! Last line: {mon.last_line}")
        else:
            print("Timeout waiting for READY")


def example_validation_script_pattern():
    """Example 6: Complete validation script pattern.

    This demonstrates the pattern used in FastLED validation scripts.
    """
    print("\nExample 6: Complete validation pattern")
    print("-" * 50)

    port = "COM13"
    test_passed = False
    errors = []

    def on_line(line: str):
        """Hook invoked for each line."""
        nonlocal test_passed, errors

        # Check for failure patterns
        if any(pattern in line for pattern in ["ERROR", "FAIL", "ASSERT"]):
            errors.append(line)

        # Check for success patterns
        if "ALL TESTS PASSED" in line:
            test_passed = True

    try:
        with SerialMonitor(port=port, baud_rate=115200, hooks=[on_line], verbose=True) as mon:
            # Send test configuration via JSON-RPC
            config_request = {
                "jsonrpc": "2.0",
                "method": "run_tests",
                "params": {"test_suite": "i2s", "timeout": 60},
                "id": 1,
            }

            print("Configuring device for tests...")
            response = mon.write_json_rpc(config_request, timeout=10.0)

            if not response or response.get("error"):
                print(f"Configuration failed: {response}")
                return False

            print("Running tests...")

            # Monitor output for test results
            for line in mon.read_lines(timeout=120):
                print(f"[TEST] {line}")

                if test_passed:
                    print("✅ All tests passed!")
                    break

                if errors:
                    print(f"❌ Test failed: {errors[0]}")
                    return False

            return test_passed

    except Exception as e:
        print(f"Error during validation: {e}")
        return False


if __name__ == "__main__":
    if len(sys.argv) > 1:
        example_num = sys.argv[1]
        examples = {
            "1": example_basic_monitoring,
            "2": example_with_hooks,
            "3": example_json_rpc,
            "4": example_with_deploy_preemption,
            "5": example_wait_for_pattern,
            "6": example_validation_script_pattern,
        }
        if example_num in examples:
            examples[example_num]()
        else:
            print(f"Unknown example: {example_num}")
            print(f"Available: {', '.join(examples.keys())}")
    else:
        print("fbuild SerialMonitor API Examples")
        print("=" * 50)
        print("\nUsage: python serial_monitor_example.py <example_number>")
        print("\nAvailable examples:")
        print("  1 - Basic serial monitoring with timeout")
        print("  2 - Pattern matching with hooks")
        print("  3 - JSON-RPC communication")
        print("  4 - Auto-reconnect during deploy preemption")
        print("  5 - Wait for specific pattern")
        print("  6 - Complete validation script pattern")
        print("\nExample:")
        print("  python serial_monitor_example.py 1")
