"""Pytest fixtures and helpers for ESP32-S3 hardware tests.

These fixtures provide common test infrastructure for hardware tests including:
- Port discovery and management
- Daemon lifecycle control
- Port accessibility verification
- Test firmware utilities
"""

import os
import subprocess
import sys
import time
from pathlib import Path
from typing import Generator

import pytest
import serial


# Port configuration
DEFAULT_ESP32S3_PORT = "COM13"
ESP32S3_PORT_ENV_VAR = "FBUILD_ESP32S3_PORT"


@pytest.fixture(scope="session")
def esp32s3_port() -> str:
    """Discover or retrieve ESP32-S3 port from environment.

    Returns:
        Port name (e.g., "COM13" on Windows, "/dev/ttyUSB0" on Linux)

    Raises:
        RuntimeError: If port is not accessible
    """
    port = os.environ.get(ESP32S3_PORT_ENV_VAR, DEFAULT_ESP32S3_PORT)

    # Verify port is accessible
    try:
        with serial.Serial(port, 115200, timeout=1) as ser:
            pass
    except serial.SerialException as e:
        raise RuntimeError(f"ESP32-S3 port {port} is not accessible: {e}")

    return port


@pytest.fixture
def clean_daemon() -> Generator[None, None, None]:
    """Ensure fbuild daemon is stopped before and after test.

    This fixture:
    1. Kills any running fbuild daemon before test
    2. Yields to test execution
    3. Kills any running fbuild daemon after test
    4. Cleans up daemon state files
    """
    # Pre-test cleanup
    _kill_fbuild_daemon()
    _cleanup_daemon_files()

    yield

    # Post-test cleanup
    _kill_fbuild_daemon()
    _cleanup_daemon_files()


def verify_port_accessible(port: str, timeout: float = 2.0) -> bool:
    """Verify that a serial port is accessible.

    Args:
        port: Port name (e.g., "COM13")
        timeout: Timeout in seconds for port access attempt

    Returns:
        True if port is accessible, False otherwise
    """
    try:
        with serial.Serial(port, 115200, timeout=timeout) as ser:
            return True
    except serial.SerialException:
        return False
    except Exception:
        return False


def start_fbuild_daemon() -> subprocess.Popen:
    """Start fbuild daemon in background.

    Returns:
        Popen object for daemon process

    Raises:
        RuntimeError: If daemon fails to start
    """
    # Ensure FBUILD_DEV_MODE is set
    env = os.environ.copy()
    env["FBUILD_DEV_MODE"] = "1"

    # Start daemon using the same method as fbuild.daemon.client.lifecycle.start_daemon()
    # On Windows, use proper detachment flags
    creationflags = 0
    if sys.platform == "win32":
        creationflags = subprocess.CREATE_NEW_PROCESS_GROUP | subprocess.DETACHED_PROCESS

    proc = subprocess.Popen(
        [sys.executable, "-m", "fbuild.daemon.daemon", f"--spawned-by={os.getpid()}"],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        stdin=subprocess.DEVNULL,
        creationflags=creationflags,
    )

    # Wait for daemon to start (up to 5 seconds)
    for _ in range(50):
        time.sleep(0.1)
        # Check if daemon is running by looking for status file
        status_file = Path(".fbuild/daemon_dev/daemon_status.json")
        if status_file.exists():
            return proc

    # Daemon failed to start
    proc.kill()
    raise RuntimeError("Failed to start fbuild daemon")


def upload_psram_enabled_firmware(port: str, firmware_path: Path) -> bool:
    """Upload firmware with PSRAM enabled to device.

    This is used for testing PSRAM crash scenarios.

    Args:
        port: Port name
        firmware_path: Path to firmware binary

    Returns:
        True if upload succeeded, False otherwise
    """
    # TODO: Implement firmware upload using esptool
    # This will be needed for Test 1.2 (GPIO validation timeout testing)
    raise NotImplementedError("Firmware upload not yet implemented")


# Internal helper functions

def _kill_fbuild_daemon() -> None:
    """Kill any running fbuild daemon processes."""
    try:
        # Try to find and kill fbuild processes
        result = subprocess.run(
            ["ps", "aux"],
            capture_output=True,
            text=True,
            check=False,
        )

        # Look for fbuild daemon processes
        for line in result.stdout.splitlines():
            if "fbuild" in line.lower() and "daemon" in line.lower():
                # Extract PID (second column in ps aux output)
                parts = line.split()
                if len(parts) > 1:
                    try:
                        pid = int(parts[1])
                        subprocess.run(["kill", "-9", str(pid)], check=False)
                    except (ValueError, subprocess.SubprocessError):
                        pass
    except Exception:
        # Ignore errors during cleanup
        pass


def _cleanup_daemon_files() -> None:
    """Remove daemon state files."""
    try:
        daemon_dir = Path(".fbuild/daemon_dev")
        if daemon_dir.exists():
            for file in daemon_dir.iterdir():
                try:
                    file.unlink()
                except Exception:
                    pass
    except Exception:
        pass
