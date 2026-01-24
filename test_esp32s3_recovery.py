#!/usr/bin/env python3
"""
Test script for ESP32-S3 crash-loop recovery.

This script implements and tests a multi-attempt recovery strategy for
crash-looping ESP32 devices.
"""

import os
import random
import subprocess
import sys
import time
from pathlib import Path


def is_crash_loop_error(error_output: str, returncode: int) -> bool:
    """Detect if the error indicates a crash-looping device.

    Args:
        error_output: stderr from esptool
        returncode: exit code from esptool

    Returns:
        True if this looks like a crash-loop error
    """
    # Check for specific error patterns
    crash_indicators = [
        "PermissionError",
        "device attached to the system is not functioning",
        "does not recognize the command",
        "ClearCommError failed",
        "Write timeout",
        "Cannot configure port",
    ]

    return any(indicator in error_output for indicator in crash_indicators)


def attempt_connection_with_recovery(
    port: str,
    chip: str = "esp32s3",
    max_attempts: int = 20,
    min_delay_ms: int = 100,
    max_delay_ms: int = 1500,
    verbose: bool = True,
) -> tuple[bool, str]:
    """Attempt to connect to a crash-looping ESP32 device.

    This function implements a multi-attempt strategy with random delays
    to catch the device during its brief bootloader window.

    Args:
        port: Serial port (e.g., "COM13")
        chip: Chip type (e.g., "esp32s3")
        max_attempts: Maximum number of connection attempts
        min_delay_ms: Minimum delay between attempts in milliseconds
        max_delay_ms: Maximum delay between attempts in milliseconds
        verbose: Whether to print progress

    Returns:
        Tuple of (success, message)
    """
    if verbose:
        print(f"\n=== ESP32 Crash-Loop Recovery Mode ===")
        print(f"Port: {port}")
        print(f"Chip: {chip}")
        print(f"Max attempts: {max_attempts}")
        print(f"Delay range: {min_delay_ms}-{max_delay_ms}ms")
        print(f"=====================================\n")

    for attempt in range(1, max_attempts + 1):
        if verbose:
            print(f"Attempt {attempt}/{max_attempts}: Waiting for bootloader window...", flush=True)

        # Build esptool command
        cmd = [
            sys.executable,
            "-m",
            "esptool",
            "--chip",
            chip,
            "--port",
            port,
            "--before",
            "default_reset",
            "--after",
            "hard_reset",
            "read_mac",
        ]

        # Strip MSYS paths on Windows
        env = os.environ.copy()
        if sys.platform == "win32" and "PATH" in env:
            paths = env["PATH"].split(os.pathsep)
            filtered_paths = [p for p in paths if "msys" not in p.lower()]
            env["PATH"] = os.pathsep.join(filtered_paths)

        try:
            # Attempt connection
            result = subprocess.run(
                cmd,
                capture_output=True,
                text=True,
                timeout=10,
                env=env,
            )

            if result.returncode == 0:
                # Success!
                if verbose:
                    print(f"\n✓ SUCCESS on attempt {attempt}!")
                    print(f"Device connected successfully")
                    if result.stdout:
                        # Print MAC address if available
                        for line in result.stdout.split("\n"):
                            if "MAC:" in line or "mac" in line.lower():
                                print(f"  {line.strip()}")
                return True, f"Connected successfully on attempt {attempt}"

            # Check if this is a crash-loop error
            error_output = result.stderr + result.stdout
            if is_crash_loop_error(error_output, result.returncode):
                if verbose:
                    # Extract the key error message
                    for line in error_output.split("\n"):
                        if "Error" in line or "PermissionError" in line:
                            print(f"  Error: {line.strip()}")
                            break

                # Random delay before next attempt
                delay_ms = random.randint(min_delay_ms, max_delay_ms)
                time.sleep(delay_ms / 1000.0)
            else:
                # Not a crash-loop error, fail immediately
                return False, f"Non-recoverable error: {error_output[:200]}"

        except subprocess.TimeoutExpired:
            if verbose:
                print(f"  Timeout on attempt {attempt}")
            # Random delay before next attempt
            delay_ms = random.randint(min_delay_ms, max_delay_ms)
            time.sleep(delay_ms / 1000.0)

        except KeyboardInterrupt:
            print("\n\nInterrupted by user")
            return False, "Interrupted by user"

        except Exception as e:
            if verbose:
                print(f"  Exception: {e}")
            delay_ms = random.randint(min_delay_ms, max_delay_ms)
            time.sleep(delay_ms / 1000.0)

    # All attempts failed
    return False, f"Failed to connect after {max_attempts} attempts. Device may be crash-looping too quickly."


def main():
    """Main test function."""
    # Test with COM13 (ESP32-S3)
    port = "COM13"
    chip = "esp32s3"

    print("Testing ESP32-S3 crash-loop recovery mechanism")
    print("=" * 60)

    success, message = attempt_connection_with_recovery(
        port=port,
        chip=chip,
        max_attempts=20,
        min_delay_ms=100,
        max_delay_ms=1500,
        verbose=True,
    )

    print("\n" + "=" * 60)
    if success:
        print(f"✓ RECOVERY SUCCESSFUL: {message}")
        print("\nThe device is now accessible and can be programmed.")
    else:
        print(f"✗ RECOVERY FAILED: {message}")
        print("\nSuggestions:")
        print("  1. Manually hold the BOOT button and press RESET")
        print("  2. Check power supply (ensure sufficient current)")
        print("  3. Try a lower baud rate (--baud 115200 or --baud 9600)")

    return 0 if success else 1


if __name__ == "__main__":
    sys.exit(main())
