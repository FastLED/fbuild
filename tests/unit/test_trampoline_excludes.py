"""Tests for trampoline exclusion patterns."""

from pathlib import Path

from fbuild.packages.trampoline_excludes import (
    INCLUDE_NEXT_PATTERNS,
    filter_paths,
    get_exclude_patterns,
    should_exclude_path,
)


def test_get_exclude_patterns_returns_list():
    """Verify get_exclude_patterns returns a list."""
    patterns = get_exclude_patterns()
    assert isinstance(patterns, list)
    assert len(patterns) > 0


def test_newlib_excluded():
    """Verify newlib/platform_include is in exclusion patterns."""
    patterns = get_exclude_patterns()
    assert "newlib/platform_include" in patterns


def test_get_exclude_patterns_returns_copy():
    """Verify get_exclude_patterns returns a copy, not the original."""
    patterns1 = get_exclude_patterns()
    patterns1.append("test")
    patterns2 = get_exclude_patterns()
    assert "test" not in patterns2


def test_include_next_patterns_constant():
    """Verify INCLUDE_NEXT_PATTERNS constant is accessible and correct."""
    assert isinstance(INCLUDE_NEXT_PATTERNS, list)
    assert "newlib/platform_include" in INCLUDE_NEXT_PATTERNS


def test_should_exclude_path_forward_slash():
    """Verify should_exclude_path works with forward slashes."""
    path = Path("/some/path/newlib/platform_include/headers")
    assert should_exclude_path(path) is True


def test_should_exclude_path_backslash():
    """Verify should_exclude_path normalizes backslashes (Windows paths)."""
    # Simulate a Windows path with backslashes
    path = Path("C:/Users/test/.fbuild/cache/newlib/platform_include")
    assert should_exclude_path(path) is True


def test_should_exclude_path_not_excluded():
    """Verify should_exclude_path returns False for non-excluded paths."""
    path = Path("/some/path/freertos/include")
    assert should_exclude_path(path) is False


def test_filter_paths():
    """Verify filter_paths correctly separates excluded and included paths."""
    paths = [
        Path("/sdk/freertos/include"),
        Path("/sdk/newlib/platform_include"),
        Path("/sdk/esp_system/include"),
    ]

    filtered, excluded = filter_paths(paths)

    assert len(filtered) == 2
    assert len(excluded) == 1
    assert Path("/sdk/newlib/platform_include") in excluded
    assert Path("/sdk/freertos/include") in filtered
    assert Path("/sdk/esp_system/include") in filtered


def test_filter_paths_empty():
    """Verify filter_paths handles empty list."""
    filtered, excluded = filter_paths([])
    assert filtered == []
    assert excluded == []


def test_filter_paths_all_excluded():
    """Verify filter_paths handles case where all paths are excluded."""
    paths = [
        Path("/sdk/newlib/platform_include"),
    ]
    filtered, excluded = filter_paths(paths)
    assert len(filtered) == 0
    assert len(excluded) == 1


def test_filter_paths_none_excluded():
    """Verify filter_paths handles case where no paths are excluded."""
    paths = [
        Path("/sdk/freertos/include"),
        Path("/sdk/esp_system/include"),
    ]
    filtered, excluded = filter_paths(paths)
    assert len(filtered) == 2
    assert len(excluded) == 0
