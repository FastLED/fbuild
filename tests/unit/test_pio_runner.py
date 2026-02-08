"""Tests for pio_runner module - PIO subprocess runner and command construction."""

import subprocess
from pathlib import Path
from unittest.mock import MagicMock, patch

from fbuild.pio_runner import (
    _get_pio_env,
    pio_build,
    pio_deploy,
    pio_monitor,
    pio_monitor_wrapped,
    run_pio,
    run_pio_with_watchdog,
)


def _mock_iso_env() -> MagicMock:
    """Create a mock IsoEnv with .run() and .open_proc() methods."""
    iso = MagicMock()
    iso.run.return_value = subprocess.CompletedProcess([], returncode=0)
    iso.open_proc.return_value = MagicMock()
    return iso


# --- _get_pio_env tests ---


@patch("fbuild.pio_runner.IsoEnv")
@patch("fbuild.pio_runner.IsoEnvArgs")
@patch("fbuild.pio_runner.Requirements")
@patch.dict("os.environ", {"FBUILD_DEV_MODE": "1"}, clear=False)
def test_get_pio_env_respects_dev_mode(mock_requirements, mock_args, mock_iso_env):
    """_get_pio_env uses cache_dev when FBUILD_DEV_MODE=1."""
    mock_iso_env.return_value = MagicMock()
    _get_pio_env()
    # Verify the venv_path ends with cache_dev/pio_iso_env
    call_kwargs = mock_args.call_args[1]
    venv_path = call_kwargs["venv_path"]
    assert "cache_dev" in str(venv_path)
    assert str(venv_path).endswith("pio_iso_env")


@patch("fbuild.pio_runner.IsoEnv")
@patch("fbuild.pio_runner.IsoEnvArgs")
@patch("fbuild.pio_runner.Requirements")
@patch.dict("os.environ", {}, clear=False)
def test_get_pio_env_production_mode(mock_requirements, mock_args, mock_iso_env):
    """_get_pio_env uses cache (not cache_dev) when FBUILD_DEV_MODE is not set."""
    # Ensure FBUILD_DEV_MODE and FBUILD_CACHE_DIR are not set
    import os

    os.environ.pop("FBUILD_DEV_MODE", None)
    os.environ.pop("FBUILD_CACHE_DIR", None)

    mock_iso_env.return_value = MagicMock()
    _get_pio_env()
    call_kwargs = mock_args.call_args[1]
    venv_path = call_kwargs["venv_path"]
    assert "cache_dev" not in str(venv_path)
    # Should end with .fbuild/cache/pio_iso_env
    parts = Path(str(venv_path)).parts
    assert parts[-1] == "pio_iso_env"
    assert parts[-2] == "cache"


# --- run_pio tests ---


@patch("fbuild.pio_runner._get_pio_env")
@patch("fbuild.pio_runner.get_pio_safe_env", return_value={"PATH": "/usr/bin"})
def test_run_pio_basic(mock_env, mock_get_pio):
    """run_pio constructs correct command and calls iso.run()."""
    iso = _mock_iso_env()
    mock_get_pio.return_value = iso

    result = run_pio(
        args=["run", "-e", "uno"],
        project_dir=Path("/project"),
        verbose=False,
    )

    iso.run.assert_called_once()
    call_args = iso.run.call_args
    assert call_args[0][0] == ["pio", "run", "-e", "uno"]
    assert call_args[1]["check"] is False
    assert result.returncode == 0


# --- pio_build tests ---


@patch("fbuild.pio_runner.run_pio")
def test_pio_build_constructs_correct_args(mock_run):
    """pio_build constructs the exact expected pio run command."""
    mock_run.return_value = subprocess.CompletedProcess([], returncode=0)

    success = pio_build(
        project_dir=Path("/project"),
        environment="uno",
        clean=False,
        verbose=False,
        jobs=None,
        has_release=False,
        has_quick=False,
    )

    assert success is True
    mock_run.assert_called_once()
    # Verify the exact args list
    call_kwargs = mock_run.call_args[1]
    assert call_kwargs["project_dir"] == Path("/project")
    assert call_kwargs["verbose"] is False
    args_list = mock_run.call_args[0][0]
    assert args_list == ["run", "-d", str(Path("/project")), "-e", "uno"]


@patch("fbuild.pio_runner.run_pio")
def test_pio_build_verbose_appends_v_flag(mock_run):
    """pio_build appends -v flag to the exact command when verbose."""
    mock_run.return_value = subprocess.CompletedProcess([], returncode=0)

    pio_build(
        project_dir=Path("/project"),
        environment="uno",
        clean=False,
        verbose=True,
        jobs=None,
        has_release=False,
        has_quick=False,
    )

    args_list = mock_run.call_args[0][0]
    assert args_list == ["run", "-d", str(Path("/project")), "-e", "uno", "-v"]


@patch("fbuild.pio_runner.run_pio")
def test_pio_build_with_clean(mock_run):
    """pio_build runs clean target before building."""
    mock_run.return_value = subprocess.CompletedProcess([], returncode=0)

    success = pio_build(
        project_dir=Path("/project"),
        environment="uno",
        clean=True,
        verbose=False,
        jobs=None,
        has_release=False,
        has_quick=False,
    )

    assert success is True
    assert mock_run.call_count == 2  # clean + build
    # First call should be clean
    clean_args = mock_run.call_args_list[0][0][0]
    assert clean_args == ["run", "--target", "clean", "-d", str(Path("/project")), "-e", "uno"]
    # Second call should be build
    build_args = mock_run.call_args_list[1][0][0]
    assert build_args == ["run", "-d", str(Path("/project")), "-e", "uno"]


@patch("fbuild.pio_runner.run_pio")
def test_pio_build_failure(mock_run):
    """pio_build returns False on non-zero exit code."""
    mock_run.return_value = subprocess.CompletedProcess([], returncode=1)

    success = pio_build(
        project_dir=Path("/project"),
        environment="uno",
        clean=False,
        verbose=False,
        jobs=None,
        has_release=False,
        has_quick=False,
    )

    assert success is False


# --- pio_deploy tests ---


@patch("fbuild.pio_runner.run_pio")
def test_pio_deploy_basic(mock_run):
    """pio_deploy constructs upload command."""
    mock_run.return_value = subprocess.CompletedProcess([], returncode=0)

    with patch("fbuild.pio_runner.sys") as mock_sys:
        mock_sys.platform = "linux"
        success = pio_deploy(
            project_dir=Path("/project"),
            environment="esp32dev",
            port=None,
            clean=False,
            verbose=False,
            monitor_flags=None,
        )

    assert success is True
    args_list = mock_run.call_args[0][0]
    assert args_list == ["run", "--target", "upload", "-d", str(Path("/project")), "-e", "esp32dev"]


@patch("fbuild.pio_runner.run_pio")
def test_pio_deploy_with_port(mock_run):
    """pio_deploy passes --upload-port when port is specified."""
    mock_run.return_value = subprocess.CompletedProcess([], returncode=0)

    with patch("fbuild.pio_runner.sys") as mock_sys:
        mock_sys.platform = "linux"
        pio_deploy(
            project_dir=Path("/project"),
            environment="esp32dev",
            port="COM13",
            clean=False,
            verbose=False,
            monitor_flags=None,
        )

    args_list = mock_run.call_args[0][0]
    assert "--upload-port" in args_list
    assert "COM13" in args_list


@patch("fbuild.pio_runner.pio_monitor", return_value=True)
@patch("fbuild.pio_runner.run_pio")
def test_pio_deploy_chains_to_monitor(mock_run, mock_monitor):
    """pio_deploy chains to pio_monitor when monitor_flags is not None."""
    mock_run.return_value = subprocess.CompletedProcess([], returncode=0)

    with patch("fbuild.pio_runner.sys") as mock_sys:
        mock_sys.platform = "linux"
        success = pio_deploy(
            project_dir=Path("/project"),
            environment="esp32dev",
            port="COM3",
            clean=False,
            verbose=False,
            monitor_flags="",  # empty string = monitor with defaults
        )

    assert success is True
    mock_monitor.assert_called_once_with(
        project_dir=Path("/project"),
        environment="esp32dev",
        port="COM3",
        baud=None,
        verbose=False,
    )


@patch("fbuild.pio_runner.pio_monitor", return_value=True)
@patch("fbuild.pio_runner.run_pio")
def test_pio_deploy_no_monitor_when_none(mock_run, mock_monitor):
    """pio_deploy does NOT chain to monitor when monitor_flags is None."""
    mock_run.return_value = subprocess.CompletedProcess([], returncode=0)

    with patch("fbuild.pio_runner.sys") as mock_sys:
        mock_sys.platform = "linux"
        success = pio_deploy(
            project_dir=Path("/project"),
            environment="esp32dev",
            port=None,
            clean=False,
            verbose=False,
            monitor_flags=None,
        )

    assert success is True
    mock_monitor.assert_not_called()


# --- pio_monitor tests ---


@patch("fbuild.pio_runner._get_pio_env")
@patch("fbuild.pio_runner.get_pio_safe_env", return_value={"PATH": "/usr/bin"})
def test_pio_monitor_basic(mock_env, mock_get_pio):
    """pio_monitor constructs device monitor command."""
    iso = _mock_iso_env()
    mock_get_pio.return_value = iso

    success = pio_monitor(
        project_dir=Path("/project"),
        environment="esp32dev",
        port=None,
        baud=None,
        verbose=False,
    )

    assert success is True
    call_args = iso.run.call_args
    cmd = call_args[0][0]
    assert cmd == ["pio", "device", "monitor", "-d", str(Path("/project")), "-e", "esp32dev"]


@patch("fbuild.pio_runner._get_pio_env")
@patch("fbuild.pio_runner.get_pio_safe_env", return_value={"PATH": "/usr/bin"})
def test_pio_monitor_with_port_and_baud(mock_env, mock_get_pio):
    """pio_monitor passes --port and --baud when specified."""
    iso = _mock_iso_env()
    mock_get_pio.return_value = iso

    pio_monitor(
        project_dir=Path("/project"),
        environment="esp32dev",
        port="COM3",
        baud=9600,
        verbose=False,
    )

    call_args = iso.run.call_args
    cmd = call_args[0][0]
    assert "--port" in cmd
    assert "COM3" in cmd
    assert "--baud" in cmd
    assert "9600" in cmd


# --- pio_monitor_wrapped tests ---


def _make_mock_proc(stdout_lines: list[bytes], returncode: int = 0) -> MagicMock:
    """Create a mock Popen process with given stdout lines."""
    proc = MagicMock()
    proc.stdout = stdout_lines  # iterable of bytes
    proc.terminate = MagicMock()
    proc.wait = MagicMock()
    proc.kill = MagicMock()
    proc.returncode = returncode
    return proc


@patch("fbuild.pio_runner.get_pio_safe_env", return_value={"PATH": "/usr/bin"})
@patch("fbuild.pio_runner._get_pio_env")
def test_monitor_wrapped_halt_on_error(mock_get_pio, _mock_env):
    """Error pattern in output triggers False return."""
    proc = _make_mock_proc(
        [
            b"Booting...\n",
            b"Running tests...\n",
            b"FATAL ERROR: segfault\n",
            b"This line should not matter\n",
        ]
    )
    iso = _mock_iso_env()
    iso.open_proc.return_value = proc
    mock_get_pio.return_value = iso

    result = pio_monitor_wrapped(
        project_dir=Path("/project"),
        environment="esp32dev",
        port=None,
        baud=None,
        verbose=False,
        timeout=None,
        halt_on_error="FATAL ERROR",
        halt_on_success=None,
        expect=None,
    )

    assert result is False
    proc.terminate.assert_called_once()


@patch("fbuild.pio_runner.get_pio_safe_env", return_value={"PATH": "/usr/bin"})
@patch("fbuild.pio_runner._get_pio_env")
def test_monitor_wrapped_halt_on_success(mock_get_pio, _mock_env):
    """Success pattern in output triggers True return."""
    proc = _make_mock_proc(
        [
            b"Booting...\n",
            b"TEST PASSED: all 10 tests\n",
            b"More output\n",
        ]
    )
    iso = _mock_iso_env()
    iso.open_proc.return_value = proc
    mock_get_pio.return_value = iso

    result = pio_monitor_wrapped(
        project_dir=Path("/project"),
        environment="esp32dev",
        port=None,
        baud=None,
        verbose=False,
        timeout=None,
        halt_on_error=None,
        halt_on_success="TEST PASSED",
        expect=None,
    )

    assert result is True
    proc.terminate.assert_called_once()


@patch("fbuild.pio_runner.get_pio_safe_env", return_value={"PATH": "/usr/bin"})
@patch("fbuild.pio_runner._get_pio_env")
@patch("fbuild.pio_runner.time")
def test_monitor_wrapped_timeout_no_expect(mock_time, mock_get_pio, _mock_env):
    """Timeout with no expect pattern returns True (completed monitoring)."""
    proc = _make_mock_proc(
        [
            b"Line 1\n",
            b"Line 2\n",
        ]
    )
    iso = _mock_iso_env()
    iso.open_proc.return_value = proc
    mock_get_pio.return_value = iso

    # First call to time.time() is start_time=0, subsequent calls simulate time passing
    mock_time.time.side_effect = [0.0, 0.5, 999.0]

    result = pio_monitor_wrapped(
        project_dir=Path("/project"),
        environment="esp32dev",
        port=None,
        baud=None,
        verbose=False,
        timeout=5,
        halt_on_error=None,
        halt_on_success=None,
        expect=None,
    )

    assert result is True
    proc.terminate.assert_called_once()


@patch("fbuild.pio_runner.get_pio_safe_env", return_value={"PATH": "/usr/bin"})
@patch("fbuild.pio_runner._get_pio_env")
@patch("fbuild.pio_runner.time")
def test_monitor_wrapped_timeout_with_expect_matched(mock_time, mock_get_pio, _mock_env):
    """Timeout + expect matched returns True."""
    proc = _make_mock_proc(
        [
            b"Line 1\n",
            b"EXPECTED OUTPUT HERE\n",
            b"Line 3\n",
        ]
    )
    iso = _mock_iso_env()
    iso.open_proc.return_value = proc
    mock_get_pio.return_value = iso

    # start_time=0, first two lines within timeout, third line triggers timeout
    mock_time.time.side_effect = [0.0, 1.0, 2.0, 999.0]

    result = pio_monitor_wrapped(
        project_dir=Path("/project"),
        environment="esp32dev",
        port=None,
        baud=None,
        verbose=False,
        timeout=5,
        halt_on_error=None,
        halt_on_success=None,
        expect="EXPECTED OUTPUT",
    )

    assert result is True


@patch("fbuild.pio_runner.get_pio_safe_env", return_value={"PATH": "/usr/bin"})
@patch("fbuild.pio_runner._get_pio_env")
@patch("fbuild.pio_runner.time")
def test_monitor_wrapped_timeout_with_expect_not_matched(mock_time, mock_get_pio, _mock_env):
    """Timeout + expect not matched returns False."""
    proc = _make_mock_proc(
        [
            b"Line 1\n",
            b"Some other output\n",
            b"Line 3\n",
        ]
    )
    iso = _mock_iso_env()
    iso.open_proc.return_value = proc
    mock_get_pio.return_value = iso

    # start_time=0, all lines within time, then third line triggers timeout
    mock_time.time.side_effect = [0.0, 1.0, 2.0, 999.0]

    result = pio_monitor_wrapped(
        project_dir=Path("/project"),
        environment="esp32dev",
        port=None,
        baud=None,
        verbose=False,
        timeout=5,
        halt_on_error=None,
        halt_on_success=None,
        expect="NEVER APPEARS",
    )

    assert result is False


@patch("fbuild.pio_runner.get_pio_safe_env", return_value={"PATH": "/usr/bin"})
@patch("fbuild.pio_runner._get_pio_env")
def test_monitor_wrapped_natural_exit_with_expect_matched(mock_get_pio, _mock_env):
    """Process ends naturally, expect matched returns True."""
    proc = _make_mock_proc(
        [
            b"Line 1\n",
            b"EXPECTED OUTPUT\n",
            b"Line 3\n",
        ]
    )
    iso = _mock_iso_env()
    iso.open_proc.return_value = proc
    mock_get_pio.return_value = iso

    result = pio_monitor_wrapped(
        project_dir=Path("/project"),
        environment="esp32dev",
        port=None,
        baud=None,
        verbose=False,
        timeout=None,
        halt_on_error=None,
        halt_on_success=None,
        expect="EXPECTED OUTPUT",
    )

    assert result is True


@patch("fbuild.pio_runner.get_pio_safe_env", return_value={"PATH": "/usr/bin"})
@patch("fbuild.pio_runner._get_pio_env")
def test_monitor_wrapped_natural_exit_with_expect_not_matched(mock_get_pio, _mock_env):
    """Process ends naturally, expect not matched returns False."""
    proc = _make_mock_proc(
        [
            b"Line 1\n",
            b"Some other output\n",
        ]
    )
    iso = _mock_iso_env()
    iso.open_proc.return_value = proc
    mock_get_pio.return_value = iso

    result = pio_monitor_wrapped(
        project_dir=Path("/project"),
        environment="esp32dev",
        port=None,
        baud=None,
        verbose=False,
        timeout=None,
        halt_on_error=None,
        halt_on_success=None,
        expect="NEVER APPEARS",
    )

    assert result is False


@patch("fbuild.pio_runner.get_pio_safe_env", return_value={"PATH": "/usr/bin"})
@patch("fbuild.pio_runner._get_pio_env")
def test_monitor_wrapped_natural_exit_no_patterns(mock_get_pio, _mock_env):
    """Process ends naturally with no patterns returns True."""
    proc = _make_mock_proc(
        [
            b"Line 1\n",
            b"Line 2\n",
        ]
    )
    iso = _mock_iso_env()
    iso.open_proc.return_value = proc
    mock_get_pio.return_value = iso

    result = pio_monitor_wrapped(
        project_dir=Path("/project"),
        environment="esp32dev",
        port=None,
        baud=None,
        verbose=False,
        timeout=None,
        halt_on_error=None,
        halt_on_success=None,
        expect=None,
    )

    assert result is True


@patch("fbuild.pio_runner.get_pio_safe_env", return_value={"PATH": "/usr/bin"})
@patch("fbuild.pio_runner._get_pio_env")
def test_monitor_wrapped_error_takes_precedence_over_success(mock_get_pio, _mock_env):
    """If error pattern appears before success pattern, returns False."""
    proc = _make_mock_proc(
        [
            b"Booting...\n",
            b"FATAL ERROR: crash\n",
            b"TEST PASSED\n",  # should not be reached
        ]
    )
    iso = _mock_iso_env()
    iso.open_proc.return_value = proc
    mock_get_pio.return_value = iso

    result = pio_monitor_wrapped(
        project_dir=Path("/project"),
        environment="esp32dev",
        port=None,
        baud=None,
        verbose=False,
        timeout=None,
        halt_on_error="FATAL ERROR",
        halt_on_success="TEST PASSED",
        expect=None,
    )

    assert result is False


# --- run_pio_with_watchdog tests ---


@patch("fbuild.pio_runner.get_pio_safe_env", return_value={"PATH": "/usr/bin"})
@patch("fbuild.pio_runner._get_pio_env")
def test_watchdog_normal_completion(mock_get_pio, _mock_env):
    """Process finishes before timeout, returns its exit code."""
    proc = MagicMock()
    proc.stdout = iter([b"line 1\n", b"line 2\n", b"done\n"])
    # poll: None while running, then 0 when done
    # The reader thread reads all stdout first, then poll returns 0
    proc.poll.return_value = 0
    proc.returncode = 0
    iso = _mock_iso_env()
    iso.open_proc.return_value = proc
    mock_get_pio.return_value = iso

    exit_code = run_pio_with_watchdog(
        args=["run", "--target", "upload", "-e", "esp32dev"],
        project_dir=Path("/project"),
        verbose=False,
        inactivity_timeout=60,
    )

    assert exit_code == 0
    proc.terminate.assert_not_called()


@patch("fbuild.pio_runner.get_pio_safe_env", return_value={"PATH": "/usr/bin"})
@patch("fbuild.pio_runner._get_pio_env")
@patch("fbuild.pio_runner.time")
def test_watchdog_inactivity_timeout(mock_time, mock_get_pio, _mock_env):
    """No output for N seconds triggers process kill, returns 1."""
    proc = MagicMock()
    # stdout that blocks (reader thread reads nothing new)
    proc.stdout = iter([])  # empty - reader thread finishes immediately
    # poll returns None (process running), and time jumps ahead
    poll_count = [0]

    def fake_poll():
        poll_count[0] += 1
        if poll_count[0] >= 3:
            # After termination, poll returns -15
            return -15
        return None

    proc.poll.side_effect = fake_poll
    proc.wait.return_value = None
    proc.returncode = -15
    iso = _mock_iso_env()
    iso.open_proc.return_value = proc
    mock_get_pio.return_value = iso

    # time.time() calls: initial last_output_time, then watchdog loop checks
    # We need the gap to exceed inactivity_timeout
    mock_time.time.side_effect = [0.0, 0.0, 100.0]
    mock_time.sleep = MagicMock()

    exit_code = run_pio_with_watchdog(
        args=["run", "--target", "upload", "-e", "esp32dev"],
        project_dir=Path("/project"),
        verbose=False,
        inactivity_timeout=10,
    )

    assert exit_code == 1
    proc.terminate.assert_called_once()
