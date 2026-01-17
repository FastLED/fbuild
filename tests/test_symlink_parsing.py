"""Unit tests for symlink:// library spec parsing."""

from pathlib import Path

import pytest

from fbuild.packages.platformio_registry import LibrarySpec


def test_symlink_with_name_format():
    """Test parsing FastLED=symlink://./ format."""
    spec = LibrarySpec.parse("FastLED=symlink://./")

    assert spec.name == "FastLED"
    assert spec.owner == ""
    assert spec.version is None
    assert spec.is_local is True
    assert spec.local_path == Path("./")


def test_symlink_with_name_relative_path():
    """Test parsing MyLib=symlink://../mylib format."""
    spec = LibrarySpec.parse("MyLib=symlink://../mylib")

    assert spec.name == "MyLib"
    assert spec.is_local is True
    assert spec.local_path == Path("../mylib")


def test_symlink_bare_format():
    """Test parsing bare symlink://../path format."""
    spec = LibrarySpec.parse("symlink://../fastled")

    assert spec.name == "fastled"
    assert spec.is_local is True
    assert spec.local_path == Path("../fastled")


def test_symlink_current_dir():
    """Test parsing symlink:// (current directory)."""
    spec = LibrarySpec.parse("symlink://./")

    assert spec.is_local is True
    assert spec.local_path == Path("./")


def test_symlink_absolute_path():
    """Test parsing symlink:///abs/path format."""
    spec = LibrarySpec.parse("symlink:///home/user/library")

    assert spec.name == "library"
    assert spec.is_local is True
    assert spec.local_path == Path("/home/user/library")


def test_symlink_windows_path():
    """Test parsing symlink://C:/path format."""
    spec = LibrarySpec.parse("MyLib=symlink://C:/dev/mylib")

    assert spec.name == "MyLib"
    assert spec.is_local is True
    assert spec.local_path == Path("C:/dev/mylib")


def test_symlink_with_spaces_in_name():
    """Test parsing with spaces around the = sign."""
    spec = LibrarySpec.parse("My Library = symlink://./path")

    assert spec.name == "My Library"
    assert spec.is_local is True
    assert spec.local_path == Path("./path")


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
