#!/usr/bin/env python3
"""
Stress test for daemon cwd fix (commit 4f7d4a8).

This script tests that the daemon correctly handles being started from various
working directories, ensuring the cwd=str(DAEMON_DIR) fix works properly.

Test scenarios:
1. Start daemon from different directories
2. Submit requests from different directories
3. Test paths with spaces
4. Test rapid daemon start/stop cycles
5. Test concurrent request submission
"""

import os
import shutil
import sys
import tempfile
import threading
import time
from pathlib import Path

from fbuild.daemon.client import (
    DAEMON_DIR,
    PID_FILE,
    STATUS_FILE,
    get_daemon_status,
    is_daemon_running,
    start_daemon,
    stop_daemon,
)


class StressTestResult:
    """Result of a stress test."""

    def __init__(self, name: str):
        self.name = name
        self.passed = False
        self.message = ""
        self.duration = 0.0

    def __repr__(self) -> str:
        status = "PASS" if self.passed else "FAIL"
        return f"[{status}] {self.name}: {self.message} ({self.duration:.2f}s)"


def kill_daemon_forcefully() -> None:
    """Force kill the daemon if running."""
    if PID_FILE.exists():
        try:
            with open(PID_FILE) as f:
                pid = int(f.read().strip())
            import psutil

            if psutil.pid_exists(pid):
                proc = psutil.Process(pid)
                proc.terminate()
                proc.wait(timeout=5)
        except Exception:
            pass
        finally:
            PID_FILE.unlink(missing_ok=True)


def wait_for_daemon_ready(timeout: float = 10.0) -> bool:
    """Wait for daemon to be ready (IDLE state)."""
    start = time.time()
    while time.time() - start < timeout:
        if is_daemon_running():
            status = get_daemon_status()
            current_status = status.get("current_status", {})
            state = current_status.get("state", "")
            if state == "idle":
                return True
        time.sleep(0.5)
    return False


def test_start_from_different_directories() -> StressTestResult:
    """Test starting the daemon from various working directories."""
    result = StressTestResult("Start from different directories")
    start_time = time.time()

    # Kill any existing daemon
    kill_daemon_forcefully()
    time.sleep(1)

    # Test directories to start from
    test_dirs = [
        Path.home(),  # Home directory
        Path("/"),  # Root
        Path(tempfile.gettempdir()),  # Temp directory
        Path(__file__).parent.parent,  # Project root
    ]

    # Add current directory
    test_dirs.append(Path.cwd())

    passed_all = True
    messages = []

    for test_dir in test_dirs:
        if not test_dir.exists():
            continue

        # Kill daemon between tests
        kill_daemon_forcefully()
        time.sleep(0.5)

        # Change to test directory and start daemon
        original_cwd = os.getcwd()
        try:
            os.chdir(str(test_dir))
            start_daemon()

            # Wait for daemon to be ready
            if wait_for_daemon_ready(timeout=15):
                messages.append(f"  OK: Started from {test_dir}")

                # Verify daemon files are in correct location
                if not PID_FILE.exists():
                    messages.append(f"  FAIL: PID file not in DAEMON_DIR when started from {test_dir}")
                    passed_all = False
                elif not STATUS_FILE.exists():
                    messages.append(f"  FAIL: Status file not in DAEMON_DIR when started from {test_dir}")
                    passed_all = False
            else:
                messages.append(f"  FAIL: Daemon didn't start from {test_dir}")
                passed_all = False

        except Exception as e:
            messages.append(f"  FAIL: Exception starting from {test_dir}: {e}")
            passed_all = False
        finally:
            os.chdir(original_cwd)

    result.passed = passed_all
    result.message = f"Tested {len(test_dirs)} directories\n" + "\n".join(messages)
    result.duration = time.time() - start_time
    return result


def test_rapid_start_stop_cycles() -> StressTestResult:
    """Test rapid daemon start/stop cycles."""
    result = StressTestResult("Rapid start/stop cycles")
    start_time = time.time()

    # Kill any existing daemon
    kill_daemon_forcefully()
    time.sleep(1)

    cycles = 5
    passed = 0
    messages = []

    for i in range(cycles):
        try:
            # Start daemon
            start_daemon()
            if wait_for_daemon_ready(timeout=10):
                # Stop daemon
                stop_daemon()
                time.sleep(1)

                # Verify it stopped
                if not is_daemon_running():
                    passed += 1
                    messages.append(f"  Cycle {i+1}: OK")
                else:
                    messages.append(f"  Cycle {i+1}: FAIL - didn't stop")
                    kill_daemon_forcefully()
            else:
                messages.append(f"  Cycle {i+1}: FAIL - didn't start")
                kill_daemon_forcefully()

            time.sleep(0.5)

        except Exception as e:
            messages.append(f"  Cycle {i+1}: FAIL - {e}")
            kill_daemon_forcefully()

    result.passed = passed == cycles
    result.message = f"Passed {passed}/{cycles} cycles\n" + "\n".join(messages)
    result.duration = time.time() - start_time
    return result


def test_path_with_spaces() -> StressTestResult:
    """Test daemon operation with paths containing spaces."""
    result = StressTestResult("Paths with spaces")
    start_time = time.time()

    # Kill any existing daemon and ensure cleanup
    kill_daemon_forcefully()
    time.sleep(2)  # Extra wait for Windows file handles to release

    # Ensure daemon files are cleaned up
    STATUS_FILE.unlink(missing_ok=True)
    time.sleep(0.5)

    # Create a temp directory with spaces in the name
    temp_base = Path(tempfile.gettempdir())
    test_dir = temp_base / "fbuild test dir with spaces"
    test_dir.mkdir(exist_ok=True)

    messages = []
    passed = True
    original_cwd = os.getcwd()

    try:
        # Change to directory with spaces
        os.chdir(str(test_dir))

        # Start daemon
        start_daemon()
        if wait_for_daemon_ready(timeout=20):
            messages.append("  OK: Daemon started from path with spaces")

            # Verify daemon files are correct
            if PID_FILE.exists() and STATUS_FILE.exists():
                messages.append("  OK: Daemon files in correct location")
            else:
                messages.append("  FAIL: Daemon files missing")
                passed = False
        else:
            # Add diagnostic info on failure
            messages.append("  FAIL: Daemon didn't start")
            messages.append(f"    - PID file exists: {PID_FILE.exists()}")
            messages.append(f"    - Status file exists: {STATUS_FILE.exists()}")
            messages.append(f"    - is_daemon_running: {is_daemon_running()}")
            if STATUS_FILE.exists():
                status = get_daemon_status()
                messages.append(f"    - Status: {status}")
            passed = False

        os.chdir(original_cwd)

    except Exception as e:
        messages.append(f"  FAIL: Exception: {e}")
        passed = False
    finally:
        # Cleanup
        try:
            os.chdir(original_cwd)
            shutil.rmtree(test_dir, ignore_errors=True)
        except Exception:
            pass

    result.passed = passed
    result.message = "\n".join(messages)
    result.duration = time.time() - start_time
    return result


def test_concurrent_requests() -> StressTestResult:
    """Test concurrent request submission from multiple threads."""
    result = StressTestResult("Concurrent requests")
    start_time = time.time()

    # Kill any existing daemon
    kill_daemon_forcefully()
    time.sleep(1)

    # Start fresh daemon
    start_daemon()
    if not wait_for_daemon_ready(timeout=15):
        result.passed = False
        result.message = "FAIL: Daemon didn't start"
        result.duration = time.time() - start_time
        return result

    # Submit requests concurrently from different "directories"
    request_results: list[tuple[int, bool, str]] = []
    lock = threading.Lock()

    def submit_status_check(thread_id: int) -> None:
        """Thread function to check daemon status."""
        try:
            # Get status from different working directory simulation
            status = get_daemon_status()
            success = status.get("running", False)
            with lock:
                request_results.append((thread_id, success, "OK" if success else "Not running"))
        except Exception as e:
            with lock:
                request_results.append((thread_id, False, str(e)))

    # Create and start threads
    threads = []
    num_threads = 10
    for i in range(num_threads):
        t = threading.Thread(target=submit_status_check, args=(i,))
        threads.append(t)
        t.start()

    # Wait for all threads
    for t in threads:
        t.join(timeout=10)

    # Analyze results
    passed_count = sum(1 for _, success, _ in request_results if success)
    messages = [f"  Thread {tid}: {'OK' if success else 'FAIL'} - {msg}" for tid, success, msg in sorted(request_results)]

    result.passed = passed_count == num_threads
    result.message = f"Passed {passed_count}/{num_threads} threads\n" + "\n".join(messages)
    result.duration = time.time() - start_time
    return result


def test_daemon_files_location() -> StressTestResult:
    """Verify daemon files are always created in DAEMON_DIR, not cwd."""
    result = StressTestResult("Daemon files location")
    start_time = time.time()

    # Kill any existing daemon
    kill_daemon_forcefully()
    time.sleep(1)

    # Create a temp directory to start from
    temp_dir = Path(tempfile.mkdtemp())
    messages = []
    passed = True
    original_cwd = os.getcwd()

    try:
        os.chdir(str(temp_dir))

        # Start daemon from temp directory
        start_daemon()
        if wait_for_daemon_ready(timeout=15):
            # Check that NO daemon files were created in temp_dir
            temp_pid = temp_dir / f"{PID_FILE.name}"
            temp_status = temp_dir / f"{STATUS_FILE.name}"

            if temp_pid.exists():
                messages.append(f"  FAIL: PID file created in cwd: {temp_pid}")
                passed = False
            else:
                messages.append("  OK: PID file NOT in cwd")

            if temp_status.exists():
                messages.append(f"  FAIL: Status file created in cwd: {temp_status}")
                passed = False
            else:
                messages.append("  OK: Status file NOT in cwd")

            # Verify files ARE in DAEMON_DIR
            if PID_FILE.exists():
                messages.append(f"  OK: PID file in DAEMON_DIR: {PID_FILE}")
            else:
                messages.append(f"  FAIL: PID file NOT in DAEMON_DIR: {PID_FILE}")
                passed = False

            if STATUS_FILE.exists():
                messages.append(f"  OK: Status file in DAEMON_DIR: {STATUS_FILE}")
            else:
                messages.append(f"  FAIL: Status file NOT in DAEMON_DIR: {STATUS_FILE}")
                passed = False
        else:
            messages.append("  FAIL: Daemon didn't start")
            passed = False

        os.chdir(original_cwd)

    except Exception as e:
        messages.append(f"  FAIL: Exception: {e}")
        passed = False
    finally:
        try:
            os.chdir(original_cwd)
            shutil.rmtree(temp_dir, ignore_errors=True)
        except Exception:
            pass

    result.passed = passed
    result.message = "\n".join(messages)
    result.duration = time.time() - start_time
    return result


def test_log_file_location() -> StressTestResult:
    """Verify log file is created in DAEMON_DIR."""
    result = StressTestResult("Log file location")
    start_time = time.time()

    LOG_FILE = DAEMON_DIR / "daemon.log"

    # Kill any existing daemon
    kill_daemon_forcefully()
    time.sleep(1)

    # Note: Don't try to delete log file on Windows - it may be locked
    # by the rotating file handler. Just check location instead.
    log_size_before = LOG_FILE.stat().st_size if LOG_FILE.exists() else 0

    # Create temp directory
    temp_dir = Path(tempfile.mkdtemp())
    messages = []
    passed = True
    original_cwd = os.getcwd()

    try:
        os.chdir(str(temp_dir))

        # Start daemon
        start_daemon()
        if wait_for_daemon_ready(timeout=15):
            # Wait a bit for log to be written
            time.sleep(2)

            # Check log file location
            temp_log = temp_dir / "daemon.log"
            if temp_log.exists():
                messages.append(f"  FAIL: Log file created in cwd: {temp_log}")
                passed = False
            else:
                messages.append("  OK: Log file NOT in cwd")

            if LOG_FILE.exists():
                messages.append(f"  OK: Log file in DAEMON_DIR: {LOG_FILE}")
                # Check log grew (new content was added)
                log_size_after = LOG_FILE.stat().st_size
                if log_size_after > log_size_before:
                    messages.append(f"  OK: Log file grew ({log_size_before} -> {log_size_after} bytes)")
                elif log_size_after > 0:
                    messages.append(f"  OK: Log file has content ({log_size_after} bytes)")
                else:
                    messages.append("  WARN: Log file is empty")
            else:
                messages.append(f"  FAIL: Log file NOT in DAEMON_DIR: {LOG_FILE}")
                passed = False
        else:
            messages.append("  FAIL: Daemon didn't start")
            passed = False

        os.chdir(original_cwd)

    except Exception as e:
        messages.append(f"  FAIL: Exception: {e}")
        passed = False
    finally:
        try:
            os.chdir(original_cwd)
            shutil.rmtree(temp_dir, ignore_errors=True)
        except Exception:
            pass

    result.passed = passed
    result.message = "\n".join(messages)
    result.duration = time.time() - start_time
    return result


def main() -> int:
    """Run all stress tests."""
    print("=" * 70)
    print("DAEMON CWD STRESS TEST")
    print("Testing fix from commit 4f7d4a8: set cwd for daemon subprocess")
    print(f"DAEMON_DIR: {DAEMON_DIR}")
    print("=" * 70)
    print()

    tests = [
        test_daemon_files_location,
        test_log_file_location,
        test_start_from_different_directories,
        test_path_with_spaces,
        test_rapid_start_stop_cycles,
        test_concurrent_requests,
    ]

    results: list[StressTestResult] = []

    for test_func in tests:
        print(f"\n{'='*60}")
        print(f"Running: {test_func.__name__}")
        print("=" * 60)

        try:
            result = test_func()
            results.append(result)
            print(result)
        except Exception as e:
            r = StressTestResult(test_func.__name__)
            r.passed = False
            r.message = f"EXCEPTION: {e}"
            results.append(r)
            print(r)

    # Cleanup
    print("\n" + "=" * 60)
    print("Cleanup: Stopping daemon...")
    kill_daemon_forcefully()

    # Summary
    print("\n" + "=" * 70)
    print("SUMMARY")
    print("=" * 70)

    passed = sum(1 for r in results if r.passed)
    total = len(results)

    for r in results:
        status = "PASS" if r.passed else "FAIL"
        print(f"[{status}] {r.name} ({r.duration:.2f}s)")

    print()
    print(f"Total: {passed}/{total} tests passed")
    print("=" * 70)

    return 0 if passed == total else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        print("\nInterrupted by user")
        kill_daemon_forcefully()
        sys.exit(130)
