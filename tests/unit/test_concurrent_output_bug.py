"""Test demonstrating the output.py concurrency bug.

This test shows that module-level globals in output.py cause
race conditions when multiple builds run concurrently.

These tests will FAIL with the current implementation (demonstrating the bug),
and will PASS after Phase 2 when output.py is refactored to use contextvars.
"""

import contextvars
import threading
import time
from unittest.mock import MagicMock

import pytest


def run_in_isolated_context(func, *args, **kwargs):
    """Run a function in an isolated context copy.

    This ensures that contextvars changes in this function don't affect
    other concurrent operations.
    """
    ctx = contextvars.copy_context()
    return ctx.run(func, *args, **kwargs)


@pytest.mark.concurrent_safety
def test_concurrent_builds_corrupt_output_globals():
    """Demonstrate race condition in output.py globals.

    When two builds run concurrently:
    1. Build A sets _output_file to A.txt
    2. Build B sets _output_file to B.txt (overwrites A's)
    3. Build A writes to _output_file (goes to B.txt - WRONG!)

    This test will FAIL until output.py is refactored to use contextvars.
    """
    from fbuild import output

    # Track which file each build wrote to
    results = {"build_a_file": None, "build_b_file": None}
    errors = []

    def simulate_build_a():
        try:
            # Build A sets its output file
            mock_file_a = MagicMock()
            mock_file_a.name = "file_A.txt"
            output.set_output_file(mock_file_a)
            time.sleep(0.01)  # Simulate work

            # Record which file is currently set (should be A, but might be B!)
            current_file = output.get_output_file()
            results["build_a_file"] = current_file.name if current_file else None
        except Exception as e:
            errors.append(f"Build A: {e}")

    def simulate_build_b():
        try:
            time.sleep(0.005)  # Start slightly after A
            # Build B sets its output file (OVERWRITES A's!)
            mock_file_b = MagicMock()
            mock_file_b.name = "file_B.txt"
            output.set_output_file(mock_file_b)
            time.sleep(0.01)  # Simulate work

            current_file = output.get_output_file()
            results["build_b_file"] = current_file.name if current_file else None
        except Exception as e:
            errors.append(f"Build B: {e}")

    # Run builds concurrently with isolated contexts
    thread_a = threading.Thread(target=run_in_isolated_context, args=(simulate_build_a,))
    thread_b = threading.Thread(target=run_in_isolated_context, args=(simulate_build_b,))
    thread_a.start()
    thread_b.start()
    thread_a.join()
    thread_b.join()

    # Check for errors
    assert not errors, f"Errors occurred: {errors}"

    # BUG: Build A should see "file_A.txt" but actually sees "file_B.txt"
    # This assertion will FAIL demonstrating the bug
    assert results["build_a_file"] == "file_A.txt", f"Race condition: Build A used {results['build_a_file']} instead of file_A.txt"
    assert results["build_b_file"] == "file_B.txt"


@pytest.mark.concurrent_safety
def test_concurrent_builds_corrupt_timestamps():
    """Demonstrate race condition in output.py timer globals.

    When two builds run concurrently:
    1. Build A calls reset_timer() at T=0
    2. Build A sleeps for 100ms
    3. Build B calls reset_timer() at T=50ms (OVERWRITES A's start time!)
    4. Build A calls get_elapsed() and sees ~50ms instead of ~100ms

    This test will FAIL until output.py is refactored to use contextvars.
    """
    from fbuild import output

    results = {"build_a_elapsed": None, "build_b_elapsed": None}

    def build_a():
        output.reset_timer()  # Sets _start_time
        time.sleep(0.1)  # Sleep 100ms
        results["build_a_elapsed"] = output.get_elapsed()

    def build_b():
        time.sleep(0.05)  # Start 50ms after A
        output.reset_timer()  # OVERWRITES A's _start_time!
        time.sleep(0.05)  # Sleep 50ms
        results["build_b_elapsed"] = output.get_elapsed()

    thread_a = threading.Thread(target=run_in_isolated_context, args=(build_a,))
    thread_b = threading.Thread(target=run_in_isolated_context, args=(build_b,))
    thread_a.start()
    thread_b.start()
    thread_a.join()
    thread_b.join()

    # Build A slept 100ms, should have ~0.1s elapsed
    # But Build B reset timer at 50ms, so A only sees ~0.05s
    # This assertion will FAIL demonstrating the bug
    assert results["build_a_elapsed"] > 0.09, f"Race condition: Build A shows {results['build_a_elapsed']}s instead of ~0.1s"


@pytest.mark.concurrent_safety
def test_concurrent_builds_corrupt_verbose_flag():
    """Demonstrate race condition in output.py verbose flag.

    When two builds run concurrently with different verbose settings:
    1. Build A sets verbose=True
    2. Build B sets verbose=False (OVERWRITES A's setting!)
    3. Build A checks verbose flag and sees False (WRONG!)

    This test will FAIL until output.py is refactored to use contextvars.
    """
    from fbuild import output

    results = {"build_a_verbose": None, "build_b_verbose": None}

    def build_a():
        output.set_verbose(True)  # Build A wants verbose output
        time.sleep(0.01)  # Simulate work
        # Check what verbose flag is set (should be True)
        ctx = output.get_context()
        results["build_a_verbose"] = ctx.verbose

    def build_b():
        time.sleep(0.005)  # Start slightly after A
        output.set_verbose(False)  # Build B wants quiet output (OVERWRITES A's!)
        time.sleep(0.01)  # Simulate work
        ctx = output.get_context()
        results["build_b_verbose"] = ctx.verbose

    thread_a = threading.Thread(target=run_in_isolated_context, args=(build_a,))
    thread_b = threading.Thread(target=run_in_isolated_context, args=(build_b,))
    thread_a.start()
    thread_b.start()
    thread_a.join()
    thread_b.join()

    # BUG: Build A should see verbose=True but actually sees verbose=False
    # This assertion will FAIL demonstrating the bug
    assert results["build_a_verbose"] is True, f"Race condition: Build A saw verbose={results['build_a_verbose']} instead of True"
    assert results["build_b_verbose"] is False
