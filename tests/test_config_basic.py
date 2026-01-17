"""Basic regression tests for config parser to ensure backward compatibility."""

from pathlib import Path
from tempfile import TemporaryDirectory

import pytest

from fbuild.config.ini_parser import PlatformIOConfig


def test_simple_uno_config():
    """Test that a simple Arduino Uno config still works."""
    with TemporaryDirectory() as tmpdir:
        ini_path = Path(tmpdir) / "platformio.ini"
        ini_content = """
[env:uno]
platform = atmelavr
board = uno
framework = arduino
build_flags = -DTEST
"""
        ini_path.write_text(ini_content)

        config = PlatformIOConfig(ini_path)

        # Test basic methods
        envs = config.get_environments()
        assert envs == ["uno"]

        # Get environment config
        uno_config = config.get_env_config("uno")
        assert uno_config["platform"] == "atmelavr"
        assert uno_config["board"] == "uno"
        assert uno_config["framework"] == "arduino"

        # Test build flags parsing
        flags = config.get_build_flags("uno")
        assert "-DTEST" in flags

        # Test lib_deps (empty)
        lib_deps = config.get_lib_deps("uno")
        assert lib_deps == []


def test_base_env_inheritance():
    """Test that [env] base section still works."""
    with TemporaryDirectory() as tmpdir:
        ini_path = Path(tmpdir) / "platformio.ini"
        ini_content = """
[env]
monitor_filters = default

[env:uno]
platform = atmelavr
board = uno
framework = arduino
"""
        ini_path.write_text(ini_content)

        config = PlatformIOConfig(ini_path)
        uno_config = config.get_env_config("uno")

        # Should inherit from [env] section
        assert uno_config["monitor_filters"] == "default"


def test_multiline_lib_deps():
    """Test that multi-line lib_deps still work."""
    with TemporaryDirectory() as tmpdir:
        ini_path = Path(tmpdir) / "platformio.ini"
        ini_content = """
[env:uno]
platform = atmelavr
board = uno
framework = arduino
lib_deps =
    https://github.com/user/lib1
    https://github.com/user/lib2
"""
        ini_path.write_text(ini_content)

        config = PlatformIOConfig(ini_path)
        lib_deps = config.get_lib_deps("uno")

        assert len(lib_deps) == 2
        assert "https://github.com/user/lib1" in lib_deps
        assert "https://github.com/user/lib2" in lib_deps


def test_default_environment():
    """Test default_envs still works."""
    with TemporaryDirectory() as tmpdir:
        ini_path = Path(tmpdir) / "platformio.ini"
        ini_content = """
[platformio]
default_envs = uno

[env:uno]
platform = atmelavr
board = uno
framework = arduino

[env:mega]
platform = atmelavr
board = mega
framework = arduino
"""
        ini_path.write_text(ini_content)

        config = PlatformIOConfig(ini_path)
        default = config.get_default_environment()

        assert default == "uno"


def test_existing_tests_uno_project():
    """Test parsing the actual tests/uno/platformio.ini if it exists."""
    uno_ini = Path("tests/uno/platformio.ini")

    if not uno_ini.exists():
        pytest.skip("tests/uno project not found")

    config = PlatformIOConfig(uno_ini)

    # Should be able to parse it
    envs = config.get_environments()
    assert "uno" in envs

    # Should be able to get the config
    uno_config = config.get_env_config("uno")
    assert uno_config["platform"] == "atmelavr"
    assert uno_config["board"] == "uno"
    assert uno_config["framework"] == "arduino"


def test_existing_tests_esp32c6_project():
    """Test parsing the actual tests/esp32c6/platformio.ini if it exists."""
    esp32c6_ini = Path("tests/esp32c6/platformio.ini")

    if not esp32c6_ini.exists():
        pytest.skip("tests/esp32c6 project not found")

    config = PlatformIOConfig(esp32c6_ini)

    # Should be able to parse it
    envs = config.get_environments()
    assert "esp32c6" in envs

    # Should be able to get the config
    esp32c6_config = config.get_env_config("esp32c6")
    assert "espressif32" in esp32c6_config["platform"]
    # Board name could be either devkitc or devkitm
    assert "esp32-c6" in esp32c6_config["board"]
    assert esp32c6_config["framework"] == "arduino"


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
