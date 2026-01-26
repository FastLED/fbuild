"""
Utilities for working with esptool and ESP32 devices.

This module provides shared functionality for ESP32 device management,
including crash-loop detection and device reset operations.
"""

import subprocess
import sys

from fbuild.subprocess_utils import get_python_executable, safe_run


def is_crash_loop_error(error_output: str) -> bool:
    """Detect if an esptool error indicates a crash-looping device.

    Crash-looping devices exhibit specific error patterns when the USB-CDC
    driver can't establish a stable connection due to rapid reboots.

    Args:
        error_output: Combined stdout/stderr from esptool

    Returns:
        True if the error indicates a crash-loop state
    """
    # Check for specific error patterns indicating crash-loop
    crash_indicators = [
        "PermissionError",
        "device attached to the system is not functioning",
        "does not recognize the command",
        "ClearCommError failed",
        "Write timeout",
        "Cannot configure port",
        "getting no sync reply",
        "timed out waiting for packet",
    ]

    return any(indicator in error_output for indicator in crash_indicators)


def reset_esp32_device(port: str, chip: str = "auto", verbose: bool = False) -> bool:
    """Reset an ESP32 device using esptool's RTS/DTR sequence.

    This function runs a minimal esptool command that triggers the hardware
    reset sequence (DTR/RTS toggling) to reset the device and release the
    USB-CDC port. This is useful when skipping firmware upload but still
    needing to ensure the port is in a clean state.

    Args:
        port: Serial port (e.g., "COM13", "/dev/ttyUSB0")
        chip: Chip type for esptool (default: "auto" for auto-detection)
        verbose: Whether to show verbose output

    Returns:
        True if reset succeeded, False otherwise
    """
    from fbuild.deploy.platform_utils import get_filtered_env

    cmd = [
        get_python_executable(),
        "-m",
        "esptool",
        "--chip",
        chip,
        "--port",
        port,
        "--before",
        "default_reset",  # Use DTR/RTS to reset chip
        "--after",
        "hard_reset",  # Reset chip after command
        "read_mac",  # Minimal command - just read MAC address
    ]

    if verbose:
        print(f"Resetting device on {port} via esptool...")

    try:
        # Use short timeout - this should be quick
        if sys.platform == "win32":
            env = get_filtered_env()
            result = safe_run(
                cmd,
                capture_output=not verbose,
                text=True,
                env=env,
                shell=False,
                timeout=15,
            )
        else:
            result = safe_run(
                cmd,
                capture_output=not verbose,
                text=True,
                timeout=15,
            )

        if result.returncode == 0:
            if verbose:
                print(f"Device on {port} reset successfully")
            return True
        else:
            if verbose:
                print(f"Device reset failed: {result.stderr}")
            return False

    except subprocess.TimeoutExpired:
        if verbose:
            print(f"Device reset timed out on {port}")
        return False
    except KeyboardInterrupt:
        import _thread

        _thread.interrupt_main()
        raise
    except Exception as e:
        if verbose:
            print(f"Device reset error: {e}")
        return False
