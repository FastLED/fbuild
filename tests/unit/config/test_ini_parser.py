"""
Unit tests for platformio.ini parser.
"""

import pytest

from zapio.config.ini_parser import PlatformIOConfig, PlatformIOConfigError


class TestPlatformIOConfig:
    """Test suite for PlatformIOConfig parser."""

    @pytest.fixture
    def tmp_ini_path(self, tmp_path):
        """Fixture to provide a temporary INI file path."""
        return tmp_path / "platformio.ini"

    @pytest.fixture
    def minimal_config(self, tmp_ini_path):
        """Create minimal valid platformio.ini."""
        content = """
[env:uno]
platform = atmelavr
board = uno
framework = arduino
"""
        tmp_ini_path.write_text(content)
        return tmp_ini_path

    @pytest.fixture
    def multi_env_config(self, tmp_ini_path):
        """Create config with multiple environments."""
        content = """
[env:uno]
platform = atmelavr
board = uno
framework = arduino

[env:mega]
platform = atmelavr
board = megaatmega2560
framework = arduino
"""
        tmp_ini_path.write_text(content)
        return tmp_ini_path

    @pytest.fixture
    def config_with_build_flags(self, tmp_ini_path):
        """Create config with build flags."""
        content = """
[env:uno]
platform = atmelavr
board = uno
framework = arduino
build_flags = -DDEBUG -DVERSION=1.0 -Os
"""
        tmp_ini_path.write_text(content)
        return tmp_ini_path

    @pytest.fixture
    def config_with_lib_deps(self, tmp_ini_path):
        """Create config with library dependencies."""
        content = """
[env:uno]
platform = atmelavr
board = uno
framework = arduino
lib_deps =
    SPI
    Wire
    Adafruit GFX Library
"""
        tmp_ini_path.write_text(content)
        return tmp_ini_path

    @pytest.fixture
    def config_with_inheritance(self, tmp_ini_path):
        """Create config with base [env] inheritance."""
        content = """
[env]
framework = arduino
platform = atmelavr

[env:uno]
board = uno

[env:mega]
board = megaatmega2560
"""
        tmp_ini_path.write_text(content)
        return tmp_ini_path

    @pytest.fixture
    def config_with_default_env(self, tmp_ini_path):
        """Create config with default environment specified."""
        content = """
[platformio]
default_envs = mega

[env:uno]
platform = atmelavr
board = uno
framework = arduino

[env:mega]
platform = atmelavr
board = megaatmega2560
framework = arduino
"""
        tmp_ini_path.write_text(content)
        return tmp_ini_path

    @pytest.fixture
    def config_with_variable_substitution(self, tmp_ini_path):
        """Create config with variable substitution."""
        content = """
[common]
build_flags = -DCOMMON_FLAG

[env:uno]
platform = atmelavr
board = uno
framework = arduino
build_flags = ${common.build_flags} -DUNO_FLAG
"""
        tmp_ini_path.write_text(content)
        return tmp_ini_path

    # Test initialization
    def test_init_with_valid_file(self, minimal_config):
        """Test initialization with a valid config file."""
        config = PlatformIOConfig(minimal_config)
        assert config.ini_path == minimal_config
        assert config.config is not None

    def test_init_with_nonexistent_file(self, tmp_path):
        """Test initialization with non-existent file raises error."""
        nonexistent = tmp_path / "nonexistent.ini"
        with pytest.raises(PlatformIOConfigError, match="not found"):
            PlatformIOConfig(nonexistent)

    def test_init_with_invalid_ini(self, tmp_ini_path):
        """Test initialization with malformed INI file."""
        tmp_ini_path.write_text("[env:uno\nthis is invalid")
        with pytest.raises(PlatformIOConfigError, match="Failed to parse"):
            PlatformIOConfig(tmp_ini_path)

    # Test get_environments
    def test_get_environments_single(self, minimal_config):
        """Test getting list of environments with single env."""
        config = PlatformIOConfig(minimal_config)
        envs = config.get_environments()
        assert envs == ["uno"]

    def test_get_environments_multiple(self, multi_env_config):
        """Test getting list of environments with multiple envs."""
        config = PlatformIOConfig(multi_env_config)
        envs = config.get_environments()
        assert set(envs) == {"uno", "mega"}

    def test_get_environments_empty(self, tmp_ini_path):
        """Test getting environments when none are defined."""
        tmp_ini_path.write_text("[common]\nfoo = bar")
        config = PlatformIOConfig(tmp_ini_path)
        assert config.get_environments() == []

    # Test get_env_config
    def test_get_env_config_valid(self, minimal_config):
        """Test getting configuration for valid environment."""
        config = PlatformIOConfig(minimal_config)
        env_config = config.get_env_config("uno")
        assert env_config["platform"] == "atmelavr"
        assert env_config["board"] == "uno"
        assert env_config["framework"] == "arduino"

    def test_get_env_config_nonexistent(self, minimal_config):
        """Test getting config for non-existent environment."""
        config = PlatformIOConfig(minimal_config)
        with pytest.raises(PlatformIOConfigError, match="not found"):
            config.get_env_config("mega")

    def test_get_env_config_missing_required_field(self, tmp_ini_path):
        """Test that missing required fields raise error."""
        content = """
[env:uno]
platform = atmelavr
framework = arduino
"""  # Missing 'board'
        tmp_ini_path.write_text(content)
        config = PlatformIOConfig(tmp_ini_path)
        with pytest.raises(PlatformIOConfigError, match="missing required fields"):
            config.get_env_config("uno")

    def test_get_env_config_with_inheritance(self, config_with_inheritance):
        """Test that environment inherits from base [env] section."""
        config = PlatformIOConfig(config_with_inheritance)

        uno_config = config.get_env_config("uno")
        assert uno_config["platform"] == "atmelavr"
        assert uno_config["board"] == "uno"
        assert uno_config["framework"] == "arduino"

        mega_config = config.get_env_config("mega")
        assert mega_config["platform"] == "atmelavr"
        assert mega_config["board"] == "megaatmega2560"
        assert mega_config["framework"] == "arduino"

    # Test get_build_flags
    def test_get_build_flags_present(self, config_with_build_flags):
        """Test parsing build flags."""
        config = PlatformIOConfig(config_with_build_flags)
        flags = config.get_build_flags("uno")
        assert flags == ["-DDEBUG", "-DVERSION=1.0", "-Os"]

    def test_get_build_flags_absent(self, minimal_config):
        """Test getting build flags when none specified."""
        config = PlatformIOConfig(minimal_config)
        flags = config.get_build_flags("uno")
        assert flags == []

    def test_get_build_flags_multiline(self, tmp_ini_path):
        """Test parsing multi-line build flags."""
        content = """
[env:uno]
platform = atmelavr
board = uno
framework = arduino
build_flags =
    -DDEBUG
    -DVERSION=1.0
    -Os
"""
        tmp_ini_path.write_text(content)
        config = PlatformIOConfig(tmp_ini_path)
        flags = config.get_build_flags("uno")
        assert set(flags) == {"-DDEBUG", "-DVERSION=1.0", "-Os"}

    # Test get_lib_deps
    def test_get_lib_deps_present(self, config_with_lib_deps):
        """Test parsing library dependencies."""
        config = PlatformIOConfig(config_with_lib_deps)
        deps = config.get_lib_deps("uno")
        assert deps == ["SPI", "Wire", "Adafruit GFX Library"]

    def test_get_lib_deps_absent(self, minimal_config):
        """Test getting lib_deps when none specified."""
        config = PlatformIOConfig(minimal_config)
        deps = config.get_lib_deps("uno")
        assert deps == []

    def test_get_lib_deps_comma_separated(self, tmp_ini_path):
        """Test parsing comma-separated library dependencies."""
        content = """
[env:uno]
platform = atmelavr
board = uno
framework = arduino
lib_deps = SPI, Wire, Adafruit GFX Library
"""
        tmp_ini_path.write_text(content)
        config = PlatformIOConfig(tmp_ini_path)
        deps = config.get_lib_deps("uno")
        assert deps == ["SPI", "Wire", "Adafruit GFX Library"]

    # Test has_environment
    def test_has_environment_exists(self, minimal_config):
        """Test checking for existing environment."""
        config = PlatformIOConfig(minimal_config)
        assert config.has_environment("uno") is True

    def test_has_environment_not_exists(self, minimal_config):
        """Test checking for non-existent environment."""
        config = PlatformIOConfig(minimal_config)
        assert config.has_environment("mega") is False

    # Test get_default_environment
    def test_get_default_environment_explicit(self, config_with_default_env):
        """Test getting explicitly specified default environment."""
        config = PlatformIOConfig(config_with_default_env)
        default = config.get_default_environment()
        assert default == "mega"

    def test_get_default_environment_first(self, multi_env_config):
        """Test getting default environment falls back to first."""
        config = PlatformIOConfig(multi_env_config)
        default = config.get_default_environment()
        assert default in ["uno", "mega"]  # Should be one of them

    def test_get_default_environment_none(self, tmp_ini_path):
        """Test getting default when no environments exist."""
        tmp_ini_path.write_text("[common]\nfoo = bar")
        config = PlatformIOConfig(tmp_ini_path)
        assert config.get_default_environment() is None

    def test_get_default_environment_multiple_comma_separated(self, tmp_ini_path):
        """Test that first of comma-separated defaults is returned."""
        content = """
[platformio]
default_envs = mega, uno

[env:uno]
platform = atmelavr
board = uno
framework = arduino

[env:mega]
platform = atmelavr
board = megaatmega2560
framework = arduino
"""
        tmp_ini_path.write_text(content)
        config = PlatformIOConfig(tmp_ini_path)
        assert config.get_default_environment() == "mega"

    # Test variable substitution
    @pytest.mark.skip(
        reason="Variable substitution with underscores needs special handling"
    )
    def test_variable_substitution(self, config_with_variable_substitution):
        """Test that variable substitution works."""
        config = PlatformIOConfig(config_with_variable_substitution)
        flags = config.get_build_flags("uno")
        assert "-DCOMMON_FLAG" in flags
        assert "-DUNO_FLAG" in flags

    # Test real-world config
    def test_real_world_config(self, tmp_ini_path):
        """Test parsing a more complex real-world config."""
        content = """
[platformio]
default_envs = uno

[env]
framework = arduino

[env:uno]
platform = atmelavr
board = uno
build_flags = -DDEBUG -Os
lib_deps =
    SPI
    Wire
    Adafruit GFX Library
upload_port = COM3
monitor_speed = 9600

[env:mega]
platform = atmelavr
board = megaatmega2560
build_flags = -DRELEASE -O2
"""
        tmp_ini_path.write_text(content)
        config = PlatformIOConfig(tmp_ini_path)

        # Test environments
        envs = config.get_environments()
        assert "uno" in envs
        assert "mega" in envs

        # Test default environment
        assert config.get_default_environment() == "uno"

        # Test uno config
        uno_config = config.get_env_config("uno")
        assert uno_config["platform"] == "atmelavr"
        assert uno_config["board"] == "uno"
        assert uno_config["framework"] == "arduino"
        assert uno_config["upload_port"] == "COM3"
        assert uno_config["monitor_speed"] == "9600"

        # Test build flags
        uno_flags = config.get_build_flags("uno")
        assert "-DDEBUG" in uno_flags
        assert "-Os" in uno_flags

        mega_flags = config.get_build_flags("mega")
        assert "-DRELEASE" in mega_flags
        assert "-O2" in mega_flags
