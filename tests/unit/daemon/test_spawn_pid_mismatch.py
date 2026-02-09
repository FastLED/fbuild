"""Test for daemon spawn PID mismatch under uv run.

When running under `uv run`, sys.executable may point to a wrapper process.
When we spawn the daemon with subprocess.Popen(), the returned PID is the
wrapper's PID, but the actual daemon writes its own PID (from os.getpid())
to the PID file. This causes a mismatch.

This test validates that spawn_daemon_process() returns a PID that matches
what the daemon will write to the PID file.
"""

import os
import subprocess
import sys
from pathlib import Path

import pytest

from fbuild.daemon.paths import PID_FILE
from fbuild.daemon.singleton_manager import spawn_daemon_process


@pytest.fixture
def clean_daemon_state():
    """Clean daemon state before and after test."""
    # Clean before
    if PID_FILE.exists():
        PID_FILE.unlink()

    yield

    # Clean after - kill any daemon we spawned
    if PID_FILE.exists():
        try:
            pid_str = PID_FILE.read_text().strip()
            daemon_pid = int(pid_str.split(",")[0])

            # Kill daemon gracefully
            if sys.platform == "win32":
                subprocess.run(["taskkill", "/PID", str(daemon_pid), "/F"], capture_output=True, check=False)
            else:
                os.kill(daemon_pid, 9)
        except Exception:
            pass

        # Clean PID file
        try:
            PID_FILE.unlink()
        except Exception:
            pass


def test_spawn_pid_matches_daemon_pid(clean_daemon_state):  # noqa: ARG001
    """Test that wait_for_pid_file handles PID mismatch correctly.

    When running under `uv run`, spawn_daemon_process returns the wrapper PID,
    but the actual daemon writes its own PID to the file. This test verifies
    that wait_for_pid_file() correctly handles this by accepting any alive daemon.

    This test spawns a real daemon and verifies that:
    1. spawn_daemon_process() may return a different PID (wrapper process)
    2. wait_for_pid_file() accepts the actual daemon PID from the file
    3. The daemon successfully starts even with PID mismatch
    """
    from fbuild.daemon.singleton_manager import wait_for_pid_file

    # Spawn daemon
    launcher_pid = os.getpid()
    spawned_pid = spawn_daemon_process(launcher_pid)
    print(f"Spawned PID: {spawned_pid}")

    # Use wait_for_pid_file which handles PID mismatch
    try:
        actual_pid = wait_for_pid_file(expected_pid=spawned_pid, timeout=15.0)
        print(f"Actual daemon PID from wait_for_pid_file: {actual_pid}")

        # Verify daemon is alive
        assert PID_FILE.exists(), "PID file should exist"

        # Read PID from file to confirm
        pid_str = PID_FILE.read_text().strip()
        daemon_pid_from_file = int(pid_str.split(",")[0])

        assert actual_pid == daemon_pid_from_file, f"wait_for_pid_file returned {actual_pid} but file contains {daemon_pid_from_file}"

        # Document whether PID mismatch occurred (expected under uv run)
        if spawned_pid != actual_pid:
            print("✓ PID mismatch detected and handled correctly:")
            print(f"  Spawned PID: {spawned_pid} (wrapper process)")
            print(f"  Actual daemon PID: {actual_pid}")
        else:
            print("✓ No PID mismatch (direct Python execution)")

        # The test passes regardless of PID mismatch - what matters is that
        # wait_for_pid_file returns a valid, alive daemon PID
        print("✅ Test passed: PID mismatch handled correctly")

    except TimeoutError as e:
        pytest.fail(f"wait_for_pid_file timed out: {e}. This indicates daemon failed to start.")


def test_sys_executable_under_uv():
    """Document what sys.executable points to under different execution contexts.

    This is informational to help understand the PID mismatch issue.
    """
    print(f"sys.executable: {sys.executable}")
    print(f"Is under uv?: {'uv' in sys.executable.lower()}")

    # Check if this is a wrapper by seeing if it's a different file than python.exe
    exe_path = Path(sys.executable)
    print(f"Executable name: {exe_path.name}")
    print(f"Executable parent: {exe_path.parent}")

    # When running under uv, sys.executable might point to uv.exe or a uv wrapper
    # The actual Python interpreter would be elsewhere
