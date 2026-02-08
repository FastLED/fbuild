"""Tests for pio_env module - MSYS environment sanitization."""

import os
from unittest.mock import patch

from fbuild.pio_env import get_pio_safe_env


def test_non_windows_returns_env_unchanged():
    """On non-Windows, get_pio_safe_env returns os.environ.copy() unchanged."""
    with patch("fbuild.pio_env.sys") as mock_sys:
        mock_sys.platform = "linux"
        env = get_pio_safe_env()
        assert isinstance(env, dict)
        # Should be a copy of os.environ
        assert env == os.environ.copy()


def test_windows_strips_msys_prefixed_vars():
    """On Windows, variables with MSYS/MINGW/CHERE prefixes are stripped."""
    test_env = {
        "MSYS_ROOT": "/usr",
        "MSYSTEM": "MINGW64",
        "MINGW_PREFIX": "/mingw64",
        "CHERE_INVOKING": "1",
        "ORIGINAL_PATH": "/usr/bin",
        "HOME": "C:\\Users\\test",
        "PATH": "C:\\Windows;C:\\Python",
    }
    with patch("fbuild.pio_env.sys") as mock_sys, patch("fbuild.pio_env.os") as mock_os:
        mock_sys.platform = "win32"
        mock_os.environ = test_env.copy()
        env = get_pio_safe_env()
        assert "MSYS_ROOT" not in env
        assert "MSYSTEM" not in env
        assert "MINGW_PREFIX" not in env
        assert "CHERE_INVOKING" not in env
        assert "ORIGINAL_PATH" not in env
        assert "HOME" in env


def test_windows_strips_exact_keys():
    """On Windows, exact keys like SHELL, SHLVL, TERM etc are stripped."""
    test_env = {
        "SHELL": "/usr/bin/bash",
        "SHLVL": "2",
        "TERM": "xterm-256color",
        "TERM_PROGRAM": "mintty",
        "TERM_PROGRAM_VERSION": "3.6",
        "TMPDIR": "/tmp",
        "HOSTTYPE": "x86_64",
        "MACHTYPE": "x86_64-pc-msys",
        "OSTYPE": "msys",
        "POSIXLY_CORRECT": "1",
        "HOME": "C:\\Users\\test",
        "PATH": "C:\\Windows",
    }
    with patch("fbuild.pio_env.sys") as mock_sys, patch("fbuild.pio_env.os") as mock_os:
        mock_sys.platform = "win32"
        mock_os.environ = test_env.copy()
        env = get_pio_safe_env()
        for key in ["SHELL", "SHLVL", "TERM", "TERM_PROGRAM", "TERM_PROGRAM_VERSION", "TMPDIR", "HOSTTYPE", "MACHTYPE", "OSTYPE", "POSIXLY_CORRECT"]:
            assert key not in env, f"{key} should be stripped"
        assert "HOME" in env


def test_windows_cleans_path_msys_entries():
    """On Windows, PATH entries starting with / are removed."""
    test_env = {
        "PATH": "C:\\Windows;/usr/bin;C:\\Python;/mingw64/bin;D:\\Tools",
    }
    with patch("fbuild.pio_env.sys") as mock_sys, patch("fbuild.pio_env.os") as mock_os:
        mock_sys.platform = "win32"
        mock_os.environ = test_env.copy()
        env = get_pio_safe_env()
        parts = env["PATH"].split(";")
        for part in parts:
            assert not part.startswith("/"), f"MSYS path should be stripped: {part}"
        assert "C:\\Windows" in parts
        assert "C:\\Python" in parts
        assert "D:\\Tools" in parts


def test_returns_copy_not_original():
    """get_pio_safe_env returns a copy, not the original environ."""
    env = get_pio_safe_env()
    assert env is not os.environ
