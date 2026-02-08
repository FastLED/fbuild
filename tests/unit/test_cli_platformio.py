"""Tests for --platformio CLI flag parsing and dispatch."""

from pathlib import Path
from unittest.mock import patch

import pytest

from fbuild.cli import (
    BuildArgs,
    DeployArgs,
    MonitorArgs,
    build_command,
    deploy_command,
    monitor_command,
    parse_default_action_args,
)


def test_build_args_has_platformio_field():
    """BuildArgs has platformio field defaulting to False."""
    args = BuildArgs(project_dir=Path("."))
    assert args.platformio is False


def test_build_args_platformio_true():
    """BuildArgs can be constructed with platformio=True."""
    args = BuildArgs(project_dir=Path("."), platformio=True)
    assert args.platformio is True


def test_deploy_args_has_platformio_field():
    """DeployArgs has platformio field defaulting to False."""
    args = DeployArgs(project_dir=Path("."))
    assert args.platformio is False


def test_deploy_args_platformio_true():
    """DeployArgs can be constructed with platformio=True."""
    args = DeployArgs(project_dir=Path("."), platformio=True)
    assert args.platformio is True


def test_monitor_args_has_platformio_field():
    """MonitorArgs has platformio field defaulting to False."""
    args = MonitorArgs(project_dir=Path("."))
    assert args.platformio is False


def test_monitor_args_platformio_true():
    """MonitorArgs can be constructed with platformio=True."""
    args = MonitorArgs(project_dir=Path("."), platformio=True)
    assert args.platformio is True


def test_parse_default_action_args_platformio(tmp_path):
    """parse_default_action_args accepts --platformio flag."""
    # Create a minimal platformio.ini so PathValidator doesn't reject
    ini = tmp_path / "platformio.ini"
    ini.write_text("[env:test]\nboard = uno\n")

    argv = ["fbuild", str(tmp_path), "--platformio"]
    result = parse_default_action_args(argv)
    assert isinstance(result, DeployArgs)
    assert result.platformio is True


def test_parse_default_action_args_no_platformio(tmp_path):
    """parse_default_action_args defaults platformio to False."""
    ini = tmp_path / "platformio.ini"
    ini.write_text("[env:test]\nboard = uno\n")

    argv = ["fbuild", str(tmp_path)]
    result = parse_default_action_args(argv)
    assert result.platformio is False


def test_parse_default_action_args_platformio_with_other_flags(tmp_path):
    """parse_default_action_args accepts --platformio alongside other flags."""
    ini = tmp_path / "platformio.ini"
    ini.write_text("[env:test]\nboard = uno\n")

    argv = ["fbuild", str(tmp_path), "-e", "test", "--platformio", "-v"]
    result = parse_default_action_args(argv)
    assert result.platformio is True
    assert result.environment == "test"
    assert result.verbose is True


# --- CLI dispatch tests ---


@patch("fbuild.cli.EnvironmentDetector.detect_environment", return_value="uno")
@patch("fbuild.pio_runner.pio_build", return_value=True)
def test_build_platformio_dispatches_to_pio_build(mock_pio_build, _mock_detect):
    """fbuild build --platformio calls pio_build, not daemon."""
    args = BuildArgs(
        project_dir=Path("/project"),
        environment="uno",
        platformio=True,
    )
    with pytest.raises(SystemExit) as exc_info:
        build_command(args)
    assert exc_info.value.code == 0
    mock_pio_build.assert_called_once()


@patch("fbuild.cli.EnvironmentDetector.detect_environment", return_value="esp32dev")
@patch("fbuild.pio_runner.pio_deploy", return_value=True)
def test_deploy_platformio_dispatches_to_pio_deploy(mock_pio_deploy, _mock_detect):
    """fbuild deploy --platformio calls pio_deploy, not daemon."""
    args = DeployArgs(
        project_dir=Path("/project"),
        environment="esp32dev",
        platformio=True,
    )
    with pytest.raises(SystemExit) as exc_info:
        deploy_command(args)
    assert exc_info.value.code == 0
    mock_pio_deploy.assert_called_once()


@patch("fbuild.cli.EnvironmentDetector.detect_environment", return_value="esp32dev")
@patch("fbuild.pio_runner.pio_monitor", return_value=True)
def test_monitor_platformio_dispatches_to_pio_monitor(mock_pio_monitor, _mock_detect):
    """fbuild monitor --platformio calls pio_monitor (no wrapping flags)."""
    args = MonitorArgs(
        project_dir=Path("/project"),
        environment="esp32dev",
        platformio=True,
    )
    with pytest.raises(SystemExit) as exc_info:
        monitor_command(args)
    assert exc_info.value.code == 0
    mock_pio_monitor.assert_called_once()


@patch("fbuild.cli.EnvironmentDetector.detect_environment", return_value="esp32dev")
@patch("fbuild.pio_runner.pio_monitor_wrapped", return_value=True)
def test_monitor_platformio_with_wrapping_dispatches_to_wrapped(mock_wrapped, _mock_detect):
    """fbuild monitor --platformio --timeout 10 calls pio_monitor_wrapped."""
    args = MonitorArgs(
        project_dir=Path("/project"),
        environment="esp32dev",
        platformio=True,
        timeout=10,
    )
    with pytest.raises(SystemExit) as exc_info:
        monitor_command(args)
    assert exc_info.value.code == 0
    mock_wrapped.assert_called_once()
    call_kwargs = mock_wrapped.call_args[1]
    assert call_kwargs["timeout"] == 10


@patch("fbuild.cli.EnvironmentDetector.detect_environment", return_value="uno")
@patch("fbuild.pio_runner.pio_build", return_value=True)
def test_build_platformio_no_release_warning_by_default(mock_pio_build, _mock_detect):
    """When --release is not explicitly passed, has_release should be False."""
    args = BuildArgs(
        project_dir=Path("/project"),
        environment="uno",
        platformio=True,
        # explicit_release defaults to False
    )
    with pytest.raises(SystemExit):
        build_command(args)
    call_kwargs = mock_pio_build.call_args[1]
    assert call_kwargs["has_release"] is False


@patch("fbuild.cli.EnvironmentDetector.detect_environment", return_value="uno")
@patch("fbuild.pio_runner.pio_build", return_value=True)
def test_build_platformio_release_warning_when_explicit(mock_pio_build, _mock_detect):
    """When --release is explicitly passed, has_release should be True."""
    args = BuildArgs(
        project_dir=Path("/project"),
        environment="uno",
        platformio=True,
        explicit_release=True,
    )
    with pytest.raises(SystemExit):
        build_command(args)
    call_kwargs = mock_pio_build.call_args[1]
    assert call_kwargs["has_release"] is True


@patch("fbuild.cli.EnvironmentDetector.detect_environment", return_value="uno")
@patch("fbuild.pio_runner.pio_build", return_value=True)
def test_build_platformio_quick_warning_when_explicit(mock_pio_build, _mock_detect):
    """When --quick is explicitly passed, has_quick should be True."""
    from fbuild.build.build_profiles import BuildProfile

    args = BuildArgs(
        project_dir=Path("/project"),
        environment="uno",
        platformio=True,
        profile=BuildProfile.QUICK,
        explicit_quick=True,
    )
    with pytest.raises(SystemExit):
        build_command(args)
    call_kwargs = mock_pio_build.call_args[1]
    assert call_kwargs["has_quick"] is True
