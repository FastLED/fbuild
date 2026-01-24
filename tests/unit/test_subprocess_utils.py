"""Tests for subprocess_utils module."""

import subprocess
from unittest.mock import patch

from fbuild.subprocess_utils import get_subprocess_creation_flags, safe_popen, safe_run


def test_get_subprocess_creation_flags_windows():
    """Test that Windows returns CREATE_NO_WINDOW flag."""
    with patch("sys.platform", "win32"):
        flags = get_subprocess_creation_flags()
        assert flags == subprocess.CREATE_NO_WINDOW


def test_get_subprocess_creation_flags_linux():
    """Test that Linux returns 0."""
    with patch("sys.platform", "linux"):
        flags = get_subprocess_creation_flags()
        assert flags == 0


@patch("subprocess.run")
def test_safe_run_applies_flags_on_windows(mock_run):
    """Test that safe_run applies flags on Windows."""
    with patch("sys.platform", "win32"):
        safe_run(["echo", "test"], capture_output=True)

        mock_run.assert_called_once()
        call_kwargs = mock_run.call_args[1]
        assert "creationflags" in call_kwargs
        assert call_kwargs["creationflags"] == subprocess.CREATE_NO_WINDOW


@patch("subprocess.run")
def test_safe_run_no_flags_on_linux(mock_run):
    """Test that safe_run doesn't apply flags on Linux."""
    with patch("sys.platform", "linux"):
        safe_run(["echo", "test"], capture_output=True)

        mock_run.assert_called_once()
        call_kwargs = mock_run.call_args[1]
        assert "creationflags" not in call_kwargs


@patch("subprocess.run")
def test_safe_run_merges_custom_creationflags(mock_run):
    """Test that custom creationflags are OR'd with defaults."""
    with patch("sys.platform", "win32"):
        custom_flag = 0x00000200  # Some custom flag
        safe_run(["echo", "test"], creationflags=custom_flag)

        mock_run.assert_called_once()
        call_kwargs = mock_run.call_args[1]
        expected = custom_flag | subprocess.CREATE_NO_WINDOW
        assert call_kwargs["creationflags"] == expected


@patch("subprocess.Popen")
def test_safe_popen_applies_flags_on_windows(mock_popen):
    """Test that safe_popen applies flags on Windows."""
    with patch("sys.platform", "win32"):
        safe_popen(["echo", "test"])

        mock_popen.assert_called_once()
        call_kwargs = mock_popen.call_args[1]
        assert "creationflags" in call_kwargs
        assert call_kwargs["creationflags"] == subprocess.CREATE_NO_WINDOW


@patch("subprocess.Popen")
def test_safe_popen_no_flags_on_linux(mock_popen):
    """Test that safe_popen doesn't apply flags on Linux."""
    with patch("sys.platform", "linux"):
        safe_popen(["echo", "test"])

        mock_popen.assert_called_once()
        call_kwargs = mock_popen.call_args[1]
        assert "creationflags" not in call_kwargs


@patch("subprocess.Popen")
def test_safe_popen_merges_custom_creationflags(mock_popen):
    """Test that custom creationflags are OR'd with defaults."""
    with patch("sys.platform", "win32"):
        custom_flag = 0x00000200  # Some custom flag
        safe_popen(["echo", "test"], creationflags=custom_flag)

        mock_popen.assert_called_once()
        call_kwargs = mock_popen.call_args[1]
        expected = custom_flag | subprocess.CREATE_NO_WINDOW
        assert call_kwargs["creationflags"] == expected
