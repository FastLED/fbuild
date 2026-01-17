"""
Tests for FastLED-required features: extends, src_dir, and board_build overrides.
"""

from pathlib import Path
from tempfile import TemporaryDirectory

import pytest

from fbuild.config.ini_parser import PlatformIOConfig, PlatformIOConfigError


def test_extends_simple_inheritance():
    """Test simple environment inheritance with extends directive."""
    with TemporaryDirectory() as tmpdir:
        ini_path = Path(tmpdir) / "platformio.ini"
        ini_content = """
[env:base]
platform = espressif32
framework = arduino
board = esp32dev
build_flags = -DBASE_FLAG

[env:child]
extends = env:base
board = esp32-c6-devkitc-1
build_flags = ${env:base.build_flags} -DCHILD_FLAG
"""
        ini_path.write_text(ini_content)

        config = PlatformIOConfig(ini_path)

        # Get child config
        child_config = config.get_env_config("child")

        # Verify inheritance
        assert child_config["platform"] == "espressif32"
        assert child_config["framework"] == "arduino"
        assert child_config["board"] == "esp32-c6-devkitc-1"
        assert "-DBASE_FLAG" in child_config["build_flags"]
        assert "-DCHILD_FLAG" in child_config["build_flags"]


def test_extends_multi_level_inheritance():
    """Test multi-level environment inheritance."""
    with TemporaryDirectory() as tmpdir:
        ini_path = Path(tmpdir) / "platformio.ini"
        ini_content = """
[env:grandparent]
platform = espressif32
framework = arduino
build_flags = -DGRAND

[env:parent]
extends = env:grandparent
board = esp32dev
build_flags = ${env:grandparent.build_flags} -DPARENT

[env:child]
extends = env:parent
board = esp32-c6-devkitc-1
build_flags = ${env:parent.build_flags} -DCHILD
"""
        ini_path.write_text(ini_content)

        config = PlatformIOConfig(ini_path)

        # Get child config
        child_config = config.get_env_config("child")

        # Verify multi-level inheritance
        assert child_config["platform"] == "espressif32"
        assert child_config["framework"] == "arduino"
        assert child_config["board"] == "esp32-c6-devkitc-1"
        assert "-DGRAND" in child_config["build_flags"]
        assert "-DPARENT" in child_config["build_flags"]
        assert "-DCHILD" in child_config["build_flags"]


def test_extends_circular_dependency_detection():
    """Test that circular dependency is detected and raises error."""
    with TemporaryDirectory() as tmpdir:
        ini_path = Path(tmpdir) / "platformio.ini"
        ini_content = """
[env:a]
platform = espressif32
framework = arduino
board = esp32dev
extends = env:b

[env:b]
extends = env:a
"""
        ini_path.write_text(ini_content)

        config = PlatformIOConfig(ini_path)

        # Should raise error for circular dependency
        with pytest.raises(PlatformIOConfigError, match="Circular dependency"):
            config.get_env_config("a")


def test_src_dir_override():
    """Test source directory override from [platformio] section."""
    with TemporaryDirectory() as tmpdir:
        ini_path = Path(tmpdir) / "platformio.ini"
        ini_content = """
[platformio]
src_dir = examples/Blink

[env:esp32]
platform = espressif32
framework = arduino
board = esp32dev
"""
        ini_path.write_text(ini_content)

        config = PlatformIOConfig(ini_path)

        # Get src_dir override
        src_dir = config.get_src_dir()

        # Verify
        assert src_dir == "examples/Blink"


def test_src_dir_not_specified():
    """Test that get_src_dir returns None when not specified."""
    with TemporaryDirectory() as tmpdir:
        ini_path = Path(tmpdir) / "platformio.ini"
        ini_content = """
[env:esp32]
platform = espressif32
framework = arduino
board = esp32dev
"""
        ini_path.write_text(ini_content)

        config = PlatformIOConfig(ini_path)

        # Get src_dir override
        src_dir = config.get_src_dir()

        # Should return None
        assert src_dir is None


def test_board_build_overrides():
    """Test board build override extraction."""
    with TemporaryDirectory() as tmpdir:
        ini_path = Path(tmpdir) / "platformio.ini"
        ini_content = """
[env:esp32c6]
platform = espressif32
framework = arduino
board = esp32-c6-devkitc-1
board_build.flash_mode = dio
board_build.flash_size = 4MB
board_build.partitions = huge_app.csv
board_upload.flash_size = 4MB
"""
        ini_path.write_text(ini_content)

        config = PlatformIOConfig(ini_path)

        # Get board overrides
        overrides = config.get_board_overrides("esp32c6")

        # Verify
        assert overrides["flash_mode"] == "dio"
        assert overrides["flash_size"] == "4MB"
        assert overrides["partitions"] == "huge_app.csv"
        assert overrides["upload_flash_size"] == "4MB"


def test_board_build_overrides_with_extends():
    """Test that board overrides work with environment inheritance."""
    with TemporaryDirectory() as tmpdir:
        ini_path = Path(tmpdir) / "platformio.ini"
        ini_content = """
[env:base]
platform = espressif32
framework = arduino
board = esp32dev

[env:child]
extends = env:base
board = esp32-c6-devkitc-1
board_build.flash_mode = dio
board_build.flash_size = 4MB
"""
        ini_path.write_text(ini_content)

        config = PlatformIOConfig(ini_path)

        # Get board overrides
        overrides = config.get_board_overrides("child")

        # Verify
        assert overrides["flash_mode"] == "dio"
        assert overrides["flash_size"] == "4MB"


def test_get_platformio_config():
    """Test getting values from [platformio] section."""
    with TemporaryDirectory() as tmpdir:
        ini_path = Path(tmpdir) / "platformio.ini"
        ini_content = """
[platformio]
src_dir = examples/Blink
build_cache_dir = .pio/build_cache
default_envs = esp32c6

[env:esp32c6]
platform = espressif32
framework = arduino
board = esp32-c6-devkitc-1
"""
        ini_path.write_text(ini_content)

        config = PlatformIOConfig(ini_path)

        # Get various platformio configs
        assert config.get_platformio_config("src_dir") == "examples/Blink"
        assert config.get_platformio_config("build_cache_dir") == ".pio/build_cache"
        assert config.get_platformio_config("default_envs") == "esp32c6"
        assert config.get_platformio_config("nonexistent") is None
        assert config.get_platformio_config("nonexistent", "default") == "default"


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
