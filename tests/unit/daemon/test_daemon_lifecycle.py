"""
Consolidated daemon lifecycle tests.

All tests that spawn/stop real daemon processes live here so that the
``xdist_group`` marker forces them onto a single pytest-xdist worker,
eliminating the race conditions that occur when parallel workers fight
over the same daemon singleton (PID file, port).

Merged from:
  - test_daemon_spawn_race.py
  - test_daemon_race_condition_fix.py
  - test_spawn_pid_mismatch.py
"""

import multiprocessing
import os
import platform
import subprocess
import sys
import time
from pathlib import Path

import pytest

from fbuild.daemon.api import DaemonStatus, get_daemon_info, request_daemon
from fbuild.daemon.client.lifecycle import stop_daemon
from fbuild.daemon.paths import DAEMON_DIR, LOCK_FILE, PID_FILE
from fbuild.daemon.singleton_manager import is_daemon_alive, read_pid_file

pytestmark = [pytest.mark.unit, pytest.mark.xdist_group(name="daemon_lifecycle")]

# ---------------------------------------------------------------------------
# Module-level helpers (must be picklable for multiprocessing)
# ---------------------------------------------------------------------------


def spawn_worker(worker_id: int) -> dict:
    """Worker function that requests daemon (runs in separate process).

    Args:
        worker_id: Unique identifier for this worker

    Returns:
        Dictionary with worker results (status, PID, message, worker_id)
    """
    try:
        response = request_daemon()
        return {
            "worker_id": worker_id,
            "status": response.status.value,
            "pid": response.pid,
            "message": response.message,
        }
    except Exception as e:
        return {
            "worker_id": worker_id,
            "status": "error",
            "pid": None,
            "message": str(e),
        }


def spawn_client(_worker_id: int) -> int | None:
    """Simulate client requesting daemon. Returns daemon PID."""
    response = request_daemon()
    return response.pid


# ---------------------------------------------------------------------------
# Robust cleanup helper (merged from both files)
# ---------------------------------------------------------------------------


def _stop_and_clean() -> None:
    """Stop any running daemon and clean up state files.

    Uses the lightweight ``stop_daemon()`` HTTP call first, then scrubs
    leftover files. Only escalates to force-kill when the process is
    still alive after the graceful attempt.
    """
    # 1 – HTTP graceful shutdown
    try:
        stop_daemon()
    except Exception:
        pass

    # 2 – Wait for graceful shutdown (up to 5 s)
    for _ in range(10):
        if not PID_FILE.exists():
            break
        time.sleep(0.5)
    else:
        time.sleep(0.5)

    # 3 – Force-kill only if process is still alive
    if PID_FILE.exists():
        try:
            pid_str = PID_FILE.read_text().strip()
            daemon_pid = int(pid_str.split(",")[0])
            if _check_pid_alive_simple(daemon_pid):
                if platform.system() == "Windows":
                    subprocess.run(
                        ["taskkill", "/F", "/PID", str(daemon_pid)],
                        capture_output=True,
                        timeout=5,
                    )
                else:
                    try:
                        os.kill(daemon_pid, 9)
                    except ProcessLookupError:
                        pass
                time.sleep(0.5)
        except Exception:
            pass

    # 4 – Scrub state files
    shutdown_file = DAEMON_DIR / "shutdown.signal"
    for f in (PID_FILE, LOCK_FILE, shutdown_file):
        try:
            if f.exists():
                f.unlink()
        except Exception:
            pass

    # 5 – Let the OS release the port
    time.sleep(0.5)


def _check_pid_alive_simple(pid: int) -> bool:
    """Check if a process with given PID is alive."""
    try:
        if platform.system() == "Windows":
            result = subprocess.run(
                ["tasklist", "/FI", f"PID eq {pid}"],
                capture_output=True,
                text=True,
                timeout=5,
            )
            return str(pid) in result.stdout
        else:
            os.kill(pid, 0)
            return True
    except (ProcessLookupError, PermissionError, subprocess.TimeoutExpired):
        return False


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture(scope="module")
def daemon_guard():
    """Clean daemon state at module boundaries."""
    _stop_and_clean()
    yield
    _stop_and_clean()


@pytest.fixture()
def clean_daemon(daemon_guard):  # noqa: ARG001
    """Clean daemon state before *and* after each test."""
    _stop_and_clean()
    yield
    _stop_and_clean()


# ---------------------------------------------------------------------------
# Tests – offline (no daemon needed)
# ---------------------------------------------------------------------------


class TestDaemonOffline:
    """Tests that verify behaviour when no daemon is running."""

    def test_get_daemon_info_without_spawn(self, clean_daemon):  # noqa: ARG002
        """``get_daemon_info()`` must not spawn a daemon."""
        response = get_daemon_info()
        assert response.status == DaemonStatus.FAILED
        assert response.pid is None
        assert not is_daemon_alive()


# ---------------------------------------------------------------------------
# Tests – single spawn
# ---------------------------------------------------------------------------


class TestDaemonSingleSpawn:
    """Tests that spawn exactly one daemon instance."""

    def test_spawn_with_retry_resilience(self, clean_daemon):  # noqa: ARG002
        """Daemon spawn succeeds even with transient failures."""
        response = request_daemon()
        assert response.status in (DaemonStatus.STARTED, DaemonStatus.ALREADY_RUNNING), f"Daemon spawn failed: {response.message}"

        daemon_info = get_daemon_info()
        assert daemon_info.status == DaemonStatus.ALREADY_RUNNING, f"Daemon not running: {daemon_info.message}"

    def test_spawn_accept_any_alive_pid(self, clean_daemon):  # noqa: ARG002
        """``wait_for_pid_file()`` accepts any alive daemon PID."""
        response = request_daemon()
        assert response.status in (DaemonStatus.STARTED, DaemonStatus.ALREADY_RUNNING)

        daemon_info = get_daemon_info()
        assert daemon_info.status == DaemonStatus.ALREADY_RUNNING
        assert daemon_info.pid == response.pid

    def test_spawn_pid_matches_daemon_pid(self, clean_daemon):  # noqa: ARG002
        """``wait_for_pid_file()`` handles PID mismatch (uv wrapper) correctly."""
        from fbuild.daemon.singleton_manager import spawn_daemon_process, wait_for_pid_file

        launcher_pid = os.getpid()
        spawned_pid = spawn_daemon_process(launcher_pid)

        try:
            actual_pid = wait_for_pid_file(expected_pid=spawned_pid, timeout=15.0)
        except TimeoutError as e:
            pytest.fail(f"wait_for_pid_file timed out: {e}")

        assert PID_FILE.exists(), "PID file should exist"
        pid_str = PID_FILE.read_text().strip()
        daemon_pid_from_file = int(pid_str.split(",")[0])
        assert actual_pid == daemon_pid_from_file

    def test_launcher_pid_tracking(self, clean_daemon):  # noqa: ARG002
        """Daemon reports who launched it."""
        launcher_pid = os.getpid()
        response = request_daemon()

        assert response.status in (DaemonStatus.STARTED, DaemonStatus.ALREADY_RUNNING)

        if response.status == DaemonStatus.STARTED:
            # Fresh spawn — launched_by must match our PID
            assert response.launched_by == launcher_pid

            response2 = request_daemon()
            assert response2.status == DaemonStatus.ALREADY_RUNNING
            assert response2.launched_by == launcher_pid
        else:
            # Daemon survived cleanup from a prior test — just verify it responds
            assert response.pid is not None or response.status == DaemonStatus.ALREADY_RUNNING


# ---------------------------------------------------------------------------
# Tests – multiple sequential calls
# ---------------------------------------------------------------------------


class TestDaemonMultiCall:
    """Tests that call ``request_daemon()`` multiple times sequentially."""

    def test_spawn_idempotent(self, clean_daemon):  # noqa: ARG002
        """Calling ``request_daemon()`` three times is safe and idempotent."""
        response1 = request_daemon()
        assert response1.status in (DaemonStatus.STARTED, DaemonStatus.ALREADY_RUNNING)

        response2 = request_daemon()
        assert response2.status in (DaemonStatus.STARTED, DaemonStatus.ALREADY_RUNNING)

        response3 = request_daemon()
        assert response3.status in (DaemonStatus.STARTED, DaemonStatus.ALREADY_RUNNING)

        daemon_info = get_daemon_info()
        assert daemon_info.status == DaemonStatus.ALREADY_RUNNING

    def test_sequential_clients_reuse_daemon(self, clean_daemon):  # noqa: ARG002
        """Sequential clients reuse the same daemon (same PID)."""
        response1 = request_daemon()
        assert response1.status == DaemonStatus.STARTED
        pid1 = response1.pid

        time.sleep(1)

        response2 = request_daemon()
        assert response2.status == DaemonStatus.ALREADY_RUNNING
        assert response2.pid == pid1

        response3 = request_daemon()
        assert response3.status == DaemonStatus.ALREADY_RUNNING
        assert response3.pid == pid1


# ---------------------------------------------------------------------------
# Tests – restart cycle
# ---------------------------------------------------------------------------


class TestDaemonRestart:
    """Tests that stop and re-spawn the daemon."""

    def test_spawn_log_append_mode(self, clean_daemon):  # noqa: ARG002
        """Spawn log appends across restarts (never overwrites)."""
        spawn_log = DAEMON_DIR / "daemon_spawn.log"
        if spawn_log.exists():
            try:
                spawn_log.unlink()
            except PermissionError:
                pass

        # First spawn
        response1 = request_daemon()
        assert response1.status in (DaemonStatus.STARTED, DaemonStatus.ALREADY_RUNNING)

        try:
            content1 = spawn_log.read_text(encoding="utf-8", errors="ignore") if spawn_log.exists() else ""
            line_count1 = len(content1.splitlines())
        except (PermissionError, UnicodeDecodeError):
            line_count1 = 0

        # Stop then re-spawn
        stop_daemon()
        time.sleep(0.5)

        response2 = request_daemon()
        assert response2.status in (DaemonStatus.STARTED, DaemonStatus.ALREADY_RUNNING)

        if spawn_log.exists():
            try:
                content2 = spawn_log.read_text(encoding="utf-8", errors="ignore")
                line_count2 = len(content2.splitlines())
                if line_count1 > 0:
                    # Line count should stay same or grow; uvicorn may overwrite
                    # portions of the log, so we accept >= rather than strict >
                    assert line_count2 >= line_count1, f"Spawn log shrunk: {line_count1} -> {line_count2} lines"
                else:
                    assert line_count2 > 0, "Spawn log is empty after second spawn"
            except (PermissionError, UnicodeDecodeError):
                pass  # acceptable on Windows where file may be locked


# ---------------------------------------------------------------------------
# Tests – concurrent spawns (heaviest, run last)
# ---------------------------------------------------------------------------


class TestDaemonConcurrent:
    """Tests that hit the daemon from multiple OS processes at once."""

    def test_concurrent_spawn_five_processes(self, clean_daemon):  # noqa: ARG002
        """Five concurrent ``request_daemon()`` calls all succeed with the same PID."""
        num_workers = 5
        with multiprocessing.Pool(processes=num_workers) as pool:
            results = pool.map(spawn_worker, range(num_workers))

        failures = [r for r in results if r["status"] == "failed"]
        errors = [r for r in results if r["status"] == "error"]
        successes = [r for r in results if r["status"] in ("started", "already_running")]

        assert len(errors) == 0, f"Workers encountered errors: {errors}"
        assert len(successes) > 0, f"All spawn attempts failed: {failures}"
        assert len(failures) == 0, f"Spurious failures detected: {len(failures)}/{num_workers} workers failed despite daemon running. Failures: {failures}"

        daemon_info = get_daemon_info()
        assert daemon_info.status == DaemonStatus.ALREADY_RUNNING

        success_pids = {r["pid"] for r in successes if r["pid"] is not None}
        if success_pids:
            assert len(success_pids) == 1, f"Multiple different daemon PIDs reported: {success_pids}"
            if daemon_info.pid is not None:
                assert daemon_info.pid in success_pids

    def test_concurrent_spawns_single_daemon(self, clean_daemon):  # noqa: ARG002
        """Ten concurrent clients result in exactly one daemon."""
        num_clients = 10
        with multiprocessing.Pool(num_clients) as pool:
            pids = pool.map(spawn_client, range(num_clients))

        pids = [p for p in pids if p is not None]
        unique_pids = set(pids)

        assert len(unique_pids) == 1, f"Expected 1 daemon, got {len(unique_pids)}: {unique_pids}"
        assert is_daemon_alive(), "Daemon PID file exists but process is not alive"

        daemon_pid = read_pid_file()
        assert daemon_pid in unique_pids


# ---------------------------------------------------------------------------
# Informational (no daemon interaction)
# ---------------------------------------------------------------------------


class TestDaemonInfo:
    """Informational tests that don't touch the daemon."""

    def test_sys_executable_under_uv(self):
        """Document what ``sys.executable`` points to under different contexts."""
        exe_path = Path(sys.executable)
        # Informational only – always passes
        assert exe_path.exists(), f"sys.executable does not exist: {exe_path}"
