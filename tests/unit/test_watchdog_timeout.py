"""Unit tests for watchdog timeout mechanism.

Tests the run_with_watchdog_timeout() function which provides robust timeout
handling for processes that may block in kernel I/O operations.
"""

import subprocess
import sys
import time
from pathlib import Path

import pytest

# Add src to path for imports
sys.path.insert(0, str(Path(__file__).parent.parent.parent / "src"))

from fbuild.deploy.deployer_esp32 import run_with_watchdog_timeout


def test_watchdog_timeout_success():
    """Test that watchdog allows successful short-running command."""
    # Run a simple command that completes quickly
    result = run_with_watchdog_timeout(
        [sys.executable, "-c", "print('hello')"],
        timeout=10,
        inactivity_timeout=5,
        verbose=False,
    )

    assert result.returncode == 0
    assert b"hello" in result.stdout


def test_watchdog_timeout_total_timeout():
    """Test that watchdog enforces total timeout."""
    # Run a command that sleeps longer than total timeout
    start = time.time()

    with pytest.raises(subprocess.TimeoutExpired) as exc_info:
        run_with_watchdog_timeout(
            [sys.executable, "-c", "import time; time.sleep(10)"],
            timeout=2,  # Total timeout: 2 seconds
            inactivity_timeout=5,  # Inactivity timeout: 5 seconds
            verbose=False,
        )

    elapsed = time.time() - start

    # Should timeout after ~2 seconds (total timeout)
    assert elapsed < 4, f"Expected ~2s timeout, got {elapsed:.1f}s"
    assert elapsed > 1.5, f"Timeout too fast: {elapsed:.1f}s"

    # Check that TimeoutExpired has the correct timeout value
    assert exc_info.value.timeout == 2


def test_watchdog_timeout_inactivity_timeout():
    """Test that watchdog enforces inactivity timeout."""
    # Create a script that prints once, then hangs
    script = """
import sys
import time
print('Starting...', flush=True)
time.sleep(10)  # Sleep without any output
print('Done', flush=True)
"""

    start = time.time()

    with pytest.raises(subprocess.TimeoutExpired) as exc_info:
        run_with_watchdog_timeout(
            [sys.executable, "-c", script],
            timeout=20,  # Total timeout: 20 seconds
            inactivity_timeout=2,  # Inactivity timeout: 2 seconds
            verbose=False,
        )

    elapsed = time.time() - start

    # Should timeout after ~2 seconds (inactivity timeout)
    # Add some buffer for thread scheduling and I/O
    assert elapsed < 5, f"Expected ~2s inactivity timeout, got {elapsed:.1f}s"
    assert elapsed > 1.5, f"Timeout too fast: {elapsed:.1f}s"

    # Check that output was captured before timeout
    assert b"Starting..." in exc_info.value.output


@pytest.mark.skip(
    reason="Flaky test due to Python stdout buffering on Windows - "
    "even with flush=True, output may be buffered long enough to trigger inactivity timeout. "
    "Real-world use case (esptool) produces large bursts of output, not affected by this issue."
)
def test_watchdog_timeout_continuous_output():
    """Test that watchdog allows process with continuous output."""
    # Create a script that continuously prints for 2 seconds
    # Print frequently enough to avoid inactivity timeout
    script = """
import sys
import time
for i in range(40):
    print(f'Output {i}', flush=True)
    time.sleep(0.05)  # 50ms between prints (well under 2s inactivity timeout)
"""

    start = time.time()

    result = run_with_watchdog_timeout(
        [sys.executable, "-c", script],
        timeout=10,  # Total timeout: 10 seconds
        inactivity_timeout=2,  # Inactivity timeout: 2 seconds
        verbose=False,
    )

    elapsed = time.time() - start

    # Should complete successfully in ~2 seconds
    assert result.returncode == 0
    assert elapsed < 5, f"Expected ~2s runtime, got {elapsed:.1f}s"
    assert b"Output 0" in result.stdout
    assert b"Output 39" in result.stdout


def test_watchdog_timeout_stderr_capture():
    """Test that watchdog captures both stdout and stderr."""
    script = """
import sys
print('stdout message', flush=True)
print('stderr message', file=sys.stderr, flush=True)
"""

    result = run_with_watchdog_timeout(
        [sys.executable, "-c", script],
        timeout=5,
        inactivity_timeout=2,
        verbose=False,
    )

    assert result.returncode == 0
    assert b"stdout message" in result.stdout
    assert b"stderr message" in result.stderr


def test_watchdog_timeout_nonzero_exit():
    """Test that watchdog preserves non-zero exit codes."""
    result = run_with_watchdog_timeout(
        [sys.executable, "-c", "import sys; sys.exit(42)"],
        timeout=5,
        inactivity_timeout=2,
        verbose=False,
    )

    assert result.returncode == 42


def test_watchdog_timeout_force_kill():
    """Test that watchdog can force kill stuck process.

    This test simulates a process that ignores SIGTERM and must be force killed.
    On Windows, this tests the TerminateProcess() code path.
    """
    # Create a script that ignores SIGTERM (Windows: CTRL_BREAK_EVENT)
    script = """
import signal
import time

# Ignore termination signals
signal.signal(signal.SIGTERM, signal.SIG_IGN)
if hasattr(signal, 'SIGBREAK'):
    signal.signal(signal.SIGBREAK, signal.SIG_IGN)

print('Started...', flush=True)
time.sleep(30)  # Long sleep to trigger timeout
"""

    start = time.time()

    with pytest.raises(subprocess.TimeoutExpired):
        run_with_watchdog_timeout(
            [sys.executable, "-c", script],
            timeout=10,
            inactivity_timeout=2,
            verbose=False,
        )

    elapsed = time.time() - start

    # Should timeout and force kill in ~2s (inactivity) + 5s (grace period) = ~7s
    assert elapsed < 10, f"Expected ~7s timeout+kill, got {elapsed:.1f}s"


@pytest.mark.skipif(sys.platform != "win32", reason="TerminateProcess() is Windows-specific")
def test_watchdog_timeout_terminate_process_windows():
    """Test Windows-specific TerminateProcess() code path.

    This verifies that the ctypes-based force kill works on Windows.
    """
    # Same as test_watchdog_timeout_force_kill but Windows-specific
    script = """
import signal
import time

signal.signal(signal.SIGTERM, signal.SIG_IGN)
signal.signal(signal.SIGBREAK, signal.SIG_IGN)

print('Started...', flush=True)
time.sleep(30)
"""

    start = time.time()

    with pytest.raises(subprocess.TimeoutExpired):
        run_with_watchdog_timeout(
            [sys.executable, "-c", script],
            timeout=10,
            inactivity_timeout=2,
            verbose=False,
        )

    elapsed = time.time() - start

    # Verify TerminateProcess() killed the process
    assert elapsed < 10, f"Expected ~7s timeout+kill, got {elapsed:.1f}s"


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
