#!/usr/bin/env python3
"""
Concurrent Deploy Test Script

This script tests the daemon's ability to handle concurrent deploy operations
as specified in Test 4.3 of docs/daemon-test-plan.md.

Usage:
    python tests/concurrent_deploy_test.py

Requirements:
    - 2x ESP32-C6 devices connected to different COM ports
    - fbuild installed and in PATH
    - Serial port permissions configured
"""

import subprocess
import sys
import time
from pathlib import Path
from typing import List, Tuple


def check_prerequisites() -> Tuple[bool, List[str]]:
    """Check if all prerequisites are met.

    Returns:
        (success, errors) tuple
    """
    errors = []

    # Check if fbuild command exists
    try:
        result = subprocess.run(
            ["fbuild", "--help"],
            capture_output=True,
            timeout=5,
        )
        if result.returncode != 0:
            errors.append("'fbuild' command not found or not working")
    except (subprocess.TimeoutExpired, FileNotFoundError):
        errors.append("'fbuild' command not found in PATH")

    # Check if test directories exist
    test_dirs = [
        Path("tests/esp32c6"),
        Path("tests/esp32c6-2"),
    ]
    for test_dir in test_dirs:
        if not test_dir.exists():
            errors.append(f"Test directory not found: {test_dir}")
        elif not (test_dir / "platformio.ini").exists():
            errors.append(f"platformio.ini not found in {test_dir}")

    # Check for serial ports
    try:
        import serial.tools.list_ports

        ports = list(serial.tools.list_ports.comports())
        if len(ports) < 2:
            errors.append(f"Need at least 2 serial ports, found {len(ports)}. " "Connect 2 ESP32-C6 devices.")
    except ImportError:
        errors.append("pyserial not installed: pip install pyserial")

    return (len(errors) == 0, errors)


def run_concurrent_deploys() -> bool:
    """Run concurrent deploy test (Test 4.3).

    Returns:
        True if test passes, False otherwise
    """
    print("=" * 80)
    print("CONCURRENT DEPLOY TEST (Test 4.3)")
    print("=" * 80)
    print()

    # Start first deploy in background
    print("Starting Deploy 1 (esp32c6, timeout=4s)...")
    proc1 = subprocess.Popen(
        [
            "fbuild",
            "deploy",
            "tests/esp32c6",
            "-e",
            "esp32c6",
            "--monitor=--timeout 4",
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )

    # Give first deploy a moment to start
    time.sleep(2)

    # Start second deploy in background
    print("Starting Deploy 2 (esp32c6-2, timeout=7s)...")
    proc2 = subprocess.Popen(
        [
            "fbuild",
            "deploy",
            "tests/esp32c6-2",
            "-e",
            "esp32c6",
            "--monitor=--timeout 7",
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )

    print()
    print("Both deploys started. Waiting for completion...")
    print()

    # Wait for both to complete
    start_time = time.time()

    # Read output from both processes
    proc1_output = []
    proc2_output = []

    while proc1.poll() is None or proc2.poll() is None:
        # Check timeout (max 5 minutes)
        if time.time() - start_time > 300:
            print("ERROR: Test timeout after 5 minutes")
            proc1.kill()
            proc2.kill()
            return False

        # Read any available output
        if proc1.poll() is None and proc1.stdout:
            line = proc1.stdout.readline()
            if line:
                proc1_output.append(line)
                print(f"[Deploy 1] {line.rstrip()}")

        if proc2.poll() is None and proc2.stdout:
            line = proc2.stdout.readline()
            if line:
                proc2_output.append(line)
                print(f"[Deploy 2] {line.rstrip()}")

        time.sleep(0.1)

    # Get final output
    if proc1.stdout:
        remaining1 = proc1.stdout.read()
        if remaining1:
            proc1_output.append(remaining1)
            print(f"[Deploy 1] {remaining1.rstrip()}")

    if proc2.stdout:
        remaining2 = proc2.stdout.read()
        if remaining2:
            proc2_output.append(remaining2)
            print(f"[Deploy 2] {remaining2.rstrip()}")

    elapsed = time.time() - start_time

    # Check results
    proc1_success = proc1.returncode == 0
    proc2_success = proc2.returncode == 0

    print()
    print("=" * 80)
    print("TEST RESULTS")
    print("=" * 80)
    print(f"Deploy 1: {'PASS' if proc1_success else 'FAIL'} (exit code: {proc1.returncode})")
    print(f"Deploy 2: {'PASS' if proc2_success else 'FAIL'} (exit code: {proc2.returncode})")
    print(f"Total time: {elapsed:.1f}s")
    print()

    # Check for errors in output
    proc1_output_str = "".join(proc1_output)
    proc2_output_str = "".join(proc2_output)

    # Look for common error patterns
    error_patterns = [
        "port locked",
        "Port .* is already in use",
        "project locked",
        "corruption",
        "ERROR",
    ]

    errors_found = []
    for pattern in error_patterns:
        if pattern.lower() in proc1_output_str.lower():
            errors_found.append(f"Deploy 1: {pattern}")
        if pattern.lower() in proc2_output_str.lower():
            errors_found.append(f"Deploy 2: {pattern}")

    if errors_found:
        print("ERRORS DETECTED:")
        for error in errors_found:
            print(f"  - {error}")
        print()

    # Determine overall pass/fail
    test_passed = proc1_success and proc2_success and not errors_found

    if test_passed:
        print("✅ TEST PASSED")
        print("   - Both deploys completed successfully")
        print("   - No port conflicts detected")
        print("   - No corruption detected")
    else:
        print("❌ TEST FAILED")
        if not proc1_success:
            print("   - Deploy 1 failed")
        if not proc2_success:
            print("   - Deploy 2 failed")
        if errors_found:
            print(f"   - {len(errors_found)} error(s) detected in output")

    print("=" * 80)
    print()

    return test_passed


def main() -> int:
    """Main entry point."""
    print()
    print("fbuild Concurrent Deploy Test")
    print()

    # Check prerequisites
    print("Checking prerequisites...")
    success, errors = check_prerequisites()

    if not success:
        print()
        print("❌ Prerequisites check FAILED:")
        for error in errors:
            print(f"   - {error}")
        print()
        print("Please fix the issues above and try again.")
        return 1

    print("✅ Prerequisites check passed")
    print()

    # Run the test
    try:
        test_passed = run_concurrent_deploys()
        return 0 if test_passed else 1
    except KeyboardInterrupt:
        print()
        print("⚠️  Test interrupted by user")
        return 130
    except Exception as e:
        print()
        print(f"❌ Unexpected error: {e}")
        import traceback

        traceback.print_exc()
        return 1


if __name__ == "__main__":
    sys.exit(main())
