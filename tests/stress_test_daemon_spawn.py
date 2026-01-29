#!/usr/bin/env python3
"""
Stress test for daemon spawn race condition handling.

This script performs intensive testing of daemon spawn logic:
1. Rapid start/stop cycles (10+ iterations)
2. Concurrent spawn from multiple processes
3. Verification of spawn logs
4. Performance metrics

Usage:
    python tests/stress_test_daemon_spawn.py
    python tests/stress_test_daemon_spawn.py --cycles 20
    python tests/stress_test_daemon_spawn.py --workers 10
"""

import argparse
import multiprocessing
import os
import sys
import time
from pathlib import Path

# Add project root to path
project_root = Path(__file__).parent.parent
sys.path.insert(0, str(project_root / "src"))

from fbuild.daemon.api import (  # noqa: E402
    DaemonStatus,
    get_daemon_info,
    request_daemon,
)
from fbuild.daemon.client.lifecycle import stop_daemon  # noqa: E402
from fbuild.daemon.paths import DAEMON_DIR, PID_FILE  # noqa: E402


def spawn_worker(worker_id: int) -> dict:
    """Worker function for concurrent spawn test."""
    start_time = time.time()
    try:
        response = request_daemon()
        elapsed = time.time() - start_time
        return {
            "worker_id": worker_id,
            "status": response.status.value,
            "pid": response.pid,
            "message": response.message,
            "elapsed": elapsed,
            "success": True,
        }
    except Exception as e:
        elapsed = time.time() - start_time
        return {
            "worker_id": worker_id,
            "status": "error",
            "pid": None,
            "message": str(e),
            "elapsed": elapsed,
            "success": False,
        }


def test_rapid_start_stop_cycles(cycles: int = 10):
    """Test rapid daemon start/stop cycles."""
    print(f"\n{'='*70}")
    print(f"Rapid Start/Stop Cycles Test ({cycles} cycles)")
    print(f"{'='*70}")

    timings = []
    errors = []

    for i in range(cycles):
        print(f"\nCycle {i+1}/{cycles}:")

        # Start daemon
        start_time = time.time()
        try:
            response = request_daemon()
            start_elapsed = time.time() - start_time

            if response.status not in (DaemonStatus.STARTED, DaemonStatus.ALREADY_RUNNING):
                errors.append(f"Cycle {i+1}: Start failed - {response.message}")
                print(f"  ❌ Start failed: {response.message}")
                continue

            print(f"  ✓ Started in {start_elapsed:.3f}s (PID {response.pid})")

            # Verify daemon is running
            daemon_info = get_daemon_info()
            if daemon_info.status != DaemonStatus.ALREADY_RUNNING:
                errors.append(f"Cycle {i+1}: Daemon not running after start")
                print("  ❌ Daemon not running after start")
                continue

            # Stop daemon
            stop_time = time.time()
            stop_daemon()
            stop_elapsed = time.time() - stop_time

            # Wait a bit for daemon to fully shut down
            time.sleep(0.5)

            # Verify daemon stopped
            daemon_info = get_daemon_info()
            if daemon_info.status != DaemonStatus.FAILED:
                errors.append(f"Cycle {i+1}: Daemon still running after stop")
                print("  ⚠ Daemon still running after stop")
            else:
                print(f"  ✓ Stopped in {stop_elapsed:.3f}s")

            timings.append(
                {
                    "cycle": i + 1,
                    "start": start_elapsed,
                    "stop": stop_elapsed,
                    "total": start_elapsed + stop_elapsed,
                }
            )

        except Exception as e:
            errors.append(f"Cycle {i+1}: Exception - {e}")
            print(f"  ❌ Exception: {e}")
            # Try to stop daemon anyway
            try:
                stop_daemon()
            except Exception:
                pass

    # Report results
    print(f"\n{'='*70}")
    print("Rapid Start/Stop Results:")
    print(f"{'='*70}")
    print(f"Total cycles: {cycles}")
    print(f"Successful: {len(timings)}/{cycles}")
    print(f"Errors: {len(errors)}/{cycles}")

    if timings:
        avg_start = sum(t["start"] for t in timings) / len(timings)
        avg_stop = sum(t["stop"] for t in timings) / len(timings)
        avg_total = sum(t["total"] for t in timings) / len(timings)

        print("\nTiming statistics:")
        print(f"  Average start time: {avg_start:.3f}s")
        print(f"  Average stop time: {avg_stop:.3f}s")
        print(f"  Average total time: {avg_total:.3f}s")

        min_start = min(t["start"] for t in timings)
        max_start = max(t["start"] for t in timings)
        print(f"  Start time range: {min_start:.3f}s - {max_start:.3f}s")

    if errors:
        print("\n⚠ Errors encountered:")
        for error in errors:
            print(f"  - {error}")

    return len(errors) == 0


def test_concurrent_spawn(num_workers: int = 5, iterations: int = 3):
    """Test concurrent spawn from multiple processes."""
    print(f"\n{'='*70}")
    print(f"Concurrent Spawn Test ({num_workers} workers, {iterations} iterations)")
    print(f"{'='*70}")

    all_results = []
    errors_by_iteration = []

    for iteration in range(iterations):
        print(f"\nIteration {iteration+1}/{iterations}:")

        # Clean state
        try:
            stop_daemon()
        except Exception:
            pass
        time.sleep(0.5)

        if PID_FILE.exists():
            try:
                PID_FILE.unlink()
            except Exception:
                pass

        # Spawn daemon from multiple processes concurrently
        start_time = time.time()
        with multiprocessing.Pool(processes=num_workers) as pool:
            results = pool.map(spawn_worker, range(num_workers))
        elapsed = time.time() - start_time

        # Analyze results
        failures = [r for r in results if r["status"] == "failed"]
        errors = [r for r in results if r["status"] == "error"]
        successes = [r for r in results if r["status"] in ("started", "already_running")]

        print(f"  Completed in {elapsed:.3f}s")
        print(f"  Successes: {len(successes)}/{num_workers}")
        print(f"  Failures: {len(failures)}/{num_workers}")
        print(f"  Errors: {len(errors)}/{num_workers}")

        # Check daemon status
        daemon_info = get_daemon_info()
        print(f"  Daemon status: {daemon_info.status.value} (PID {daemon_info.pid})")

        # Record iteration results
        iteration_errors = []
        if len(failures) > 0:
            iteration_errors.append(f"Iteration {iteration+1}: {len(failures)} spurious failures")
        if len(errors) > 0:
            iteration_errors.append(f"Iteration {iteration+1}: {len(errors)} worker errors")
        if daemon_info.status != DaemonStatus.ALREADY_RUNNING:
            iteration_errors.append(f"Iteration {iteration+1}: Daemon not running")

        errors_by_iteration.extend(iteration_errors)
        all_results.append(
            {
                "iteration": iteration + 1,
                "successes": len(successes),
                "failures": len(failures),
                "errors": len(errors),
                "elapsed": elapsed,
                "daemon_pid": daemon_info.pid,
            }
        )

        # Clean up for next iteration
        try:
            stop_daemon()
        except Exception:
            pass
        time.sleep(0.5)

    # Report results
    print(f"\n{'='*70}")
    print("Concurrent Spawn Results:")
    print(f"{'='*70}")
    print(f"Total iterations: {iterations}")

    total_successes = sum(r["successes"] for r in all_results)
    total_failures = sum(r["failures"] for r in all_results)
    total_errors = sum(r["errors"] for r in all_results)
    total_attempts = iterations * num_workers

    print(f"Total attempts: {total_attempts}")
    print(f"Total successes: {total_successes}")
    print(f"Total failures: {total_failures} (spurious errors)")
    print(f"Total errors: {total_errors}")

    avg_elapsed = sum(r["elapsed"] for r in all_results) / iterations
    print(f"\nAverage spawn time: {avg_elapsed:.3f}s")

    if errors_by_iteration:
        print("\n⚠ Errors encountered:")
        for error in errors_by_iteration:
            print(f"  - {error}")

    # Success criteria: zero spurious failures
    success = total_failures == 0 and total_errors == 0
    if success:
        print(f"\n✓ PASSED: Zero spurious failures across {iterations} iterations")
    else:
        print(f"\n❌ FAILED: {total_failures} spurious failures detected")

    return success


def check_spawn_logs():
    """Check spawn logs for issues."""
    print(f"\n{'='*70}")
    print("Spawn Log Analysis")
    print(f"{'='*70}")

    spawn_log = DAEMON_DIR / "daemon_spawn.log"

    if not spawn_log.exists():
        print("⚠ Spawn log does not exist")
        return True

    try:
        content = spawn_log.read_text(encoding="utf-8", errors="ignore")
        lines = content.splitlines()

        print(f"Log size: {len(content)} bytes")
        print(f"Line count: {len(lines)}")

        # Count spawn attempts
        spawn_headers = content.count("Spawn attempt at")
        print(f"Spawn attempts logged: {spawn_headers}")

        # Look for error patterns
        error_count = content.lower().count("error")
        exception_count = content.lower().count("exception")
        failed_count = content.lower().count("failed")

        print("\nError indicators:")
        print(f"  'error': {error_count}")
        print(f"  'exception': {exception_count}")
        print(f"  'failed': {failed_count}")

        # Show last 20 lines
        print("\nLast 20 lines of spawn log:")
        print("-" * 70)
        for line in lines[-20:]:
            print(f"  {line}")
        print("-" * 70)

        return True

    except (PermissionError, UnicodeDecodeError) as e:
        print(f"⚠ Could not read spawn log: {e}")
        return True


def main():
    parser = argparse.ArgumentParser(description="Stress test daemon spawn logic")
    parser.add_argument("--cycles", type=int, default=10, help="Number of start/stop cycles")
    parser.add_argument("--workers", type=int, default=5, help="Number of concurrent workers")
    parser.add_argument("--iterations", type=int, default=3, help="Number of concurrent spawn iterations")
    parser.add_argument("--skip-rapid", action="store_true", help="Skip rapid start/stop test")
    parser.add_argument("--skip-concurrent", action="store_true", help="Skip concurrent spawn test")
    parser.add_argument("--skip-logs", action="store_true", help="Skip log analysis")

    args = parser.parse_args()

    print(f"\n{'='*70}")
    print("Daemon Spawn Stress Test")
    print(f"{'='*70}")
    print("Configuration:")
    print(f"  Start/stop cycles: {args.cycles}")
    print(f"  Concurrent workers: {args.workers}")
    print(f"  Concurrent iterations: {args.iterations}")
    print(f"  FBUILD_DEV_MODE: {os.getenv('FBUILD_DEV_MODE', 'not set')}")

    results = []

    # Test 1: Rapid start/stop cycles
    if not args.skip_rapid:
        success = test_rapid_start_stop_cycles(args.cycles)
        results.append(("Rapid start/stop", success))

    # Test 2: Concurrent spawn
    if not args.skip_concurrent:
        success = test_concurrent_spawn(args.workers, args.iterations)
        results.append(("Concurrent spawn", success))

    # Test 3: Check spawn logs
    if not args.skip_logs:
        success = check_spawn_logs()
        results.append(("Spawn log analysis", success))

    # Final summary
    print(f"\n{'='*70}")
    print("FINAL SUMMARY")
    print(f"{'='*70}")

    for test_name, success in results:
        status = "✓ PASSED" if success else "❌ FAILED"
        print(f"{status}: {test_name}")

    all_passed = all(success for _, success in results)

    if all_passed:
        print("\n✓ ALL TESTS PASSED")
        return 0
    else:
        print("\n❌ SOME TESTS FAILED")
        return 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        print("\n\nInterrupted by user")
        sys.exit(1)
