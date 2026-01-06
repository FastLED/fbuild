"""
Unit tests for board configuration loader.
"""

from pathlib import Path

import pytest

from zapio.config.board_config import BoardConfig, BoardConfigError


class TestBoardConfig:
    """Test suite for BoardConfig loader."""

    @pytest.fixture
    def tmp_boards_txt(self, tmp_path):
        """Fixture to provide a temporary boards.txt file path."""
        return tmp_path / "boards.txt"

    @pytest.fixture
    def minimal_boards_txt(self, tmp_boards_txt):
        """Create minimal valid boards.txt."""
        content = """
# Arduino Uno
uno.name=Arduino Uno
uno.build.mcu=atmega328p
uno.build.f_cpu=16000000L
uno.build.board=AVR_UNO
uno.build.core=arduino
uno.build.variant=standard
"""
        tmp_boards_txt.write_text(content)
        return tmp_boards_txt

    @pytest.fixture
    def multi_board_boards_txt(self, tmp_boards_txt):
        """Create boards.txt with multiple boards."""
        content = """
# Arduino Uno
uno.name=Arduino Uno
uno.build.mcu=atmega328p
uno.build.f_cpu=16000000L
uno.build.board=AVR_UNO
uno.build.core=arduino
uno.build.variant=standard

# Arduino Mega
mega.name=Arduino Mega 2560
mega.build.mcu=atmega2560
mega.build.f_cpu=16000000L
mega.build.board=AVR_MEGA2560
mega.build.core=arduino
mega.build.variant=mega
"""
        tmp_boards_txt.write_text(content)
        return tmp_boards_txt

    @pytest.fixture
    def boards_txt_with_upload_info(self, tmp_boards_txt):
        """Create boards.txt with upload information."""
        content = """
uno.name=Arduino Uno
uno.build.mcu=atmega328p
uno.build.f_cpu=16000000L
uno.build.board=AVR_UNO
uno.build.core=arduino
uno.build.variant=standard
uno.upload.tool=avrdude
uno.upload.protocol=arduino
uno.upload.maximum_size=32256
uno.upload.speed=115200
uno.vid=0x2341
uno.pid=0x0043
"""
        tmp_boards_txt.write_text(content)
        return tmp_boards_txt

    # Test initialization
    def test_init_with_required_fields(self):
        """Test initialization with required fields."""
        config = BoardConfig(
            name="Arduino Uno", mcu="atmega328p", f_cpu="16000000L", board="AVR_UNO"
        )
        assert config.name == "Arduino Uno"
        assert config.mcu == "atmega328p"
        assert config.f_cpu == "16000000L"
        assert config.board == "AVR_UNO"
        assert config.core == "arduino"  # default
        assert config.variant == "standard"  # default

    def test_init_with_all_fields(self):
        """Test initialization with all optional fields."""
        config = BoardConfig(
            name="Arduino Uno",
            mcu="atmega328p",
            f_cpu="16000000L",
            board="AVR_UNO",
            core="arduino",
            variant="standard",
            vid="0x2341",
            pid="0x0043",
            extra_flags={"upload.speed": "115200"},
        )
        assert config.vid == "0x2341"
        assert config.pid == "0x0043"
        assert config.extra_flags["upload.speed"] == "115200"

    # Test from_boards_txt
    def test_from_boards_txt_valid(self, minimal_boards_txt):
        """Test loading from valid boards.txt."""
        config = BoardConfig.from_boards_txt(minimal_boards_txt, "uno")
        assert config.name == "Arduino Uno"
        assert config.mcu == "atmega328p"
        assert config.f_cpu == "16000000L"
        assert config.board == "AVR_UNO"
        assert config.core == "arduino"
        assert config.variant == "standard"

    def test_from_boards_txt_nonexistent_file(self, tmp_path):
        """Test loading from non-existent boards.txt."""
        nonexistent = tmp_path / "nonexistent.txt"
        with pytest.raises(BoardConfigError, match="not found"):
            BoardConfig.from_boards_txt(nonexistent, "uno")

    def test_from_boards_txt_nonexistent_board(self, minimal_boards_txt):
        """Test loading non-existent board from boards.txt."""
        with pytest.raises(BoardConfigError, match="not found"):
            BoardConfig.from_boards_txt(minimal_boards_txt, "mega")

    def test_from_boards_txt_missing_required_field(self, tmp_boards_txt):
        """Test loading board with missing required field."""
        content = """
uno.name=Arduino Uno
uno.build.mcu=atmega328p
uno.build.f_cpu=16000000L
# Missing uno.build.board
"""
        tmp_boards_txt.write_text(content)
        with pytest.raises(BoardConfigError, match="missing required field"):
            BoardConfig.from_boards_txt(tmp_boards_txt, "uno")

    def test_from_boards_txt_with_overrides(self, minimal_boards_txt):
        """Test loading with overrides."""
        overrides = {
            "mcu": "atmega328pb",  # Override MCU
            "f_cpu": "8000000L",  # Override frequency
        }
        config = BoardConfig.from_boards_txt(minimal_boards_txt, "uno", overrides)
        assert config.mcu == "atmega328pb"
        assert config.f_cpu == "8000000L"
        assert config.board == "AVR_UNO"  # Not overridden

    def test_from_boards_txt_multiple_boards(self, multi_board_boards_txt):
        """Test loading different boards from same file."""
        uno_config = BoardConfig.from_boards_txt(multi_board_boards_txt, "uno")
        assert uno_config.name == "Arduino Uno"
        assert uno_config.mcu == "atmega328p"
        assert uno_config.variant == "standard"

        mega_config = BoardConfig.from_boards_txt(multi_board_boards_txt, "mega")
        assert mega_config.name == "Arduino Mega 2560"
        assert mega_config.mcu == "atmega2560"
        assert mega_config.variant == "mega"

    def test_from_boards_txt_with_upload_info(self, boards_txt_with_upload_info):
        """Test that upload information is captured."""
        config = BoardConfig.from_boards_txt(boards_txt_with_upload_info, "uno")
        assert config.vid == "0x2341"
        assert config.pid == "0x0043"
        # Upload info should be in extra_flags
        assert "tool" in config.extra_flags
        assert config.extra_flags["tool"] == "avrdude"

    # Test from_board_id
    def test_from_board_id_uno(self):
        """Test loading Uno from built-in defaults."""
        config = BoardConfig.from_board_id("uno")
        assert config.name == "Arduino Uno"
        assert config.mcu == "atmega328p"
        assert config.f_cpu == "16000000L"
        assert config.board == "AVR_UNO"
        assert config.core == "arduino"
        assert config.variant == "standard"

    def test_from_board_id_mega(self):
        """Test loading Mega from built-in defaults."""
        config = BoardConfig.from_board_id("mega")
        assert config.name == "Arduino Mega"
        assert config.mcu == "atmega2560"
        assert config.board == "AVR_MEGA2560"
        assert config.variant == "mega"

    def test_from_board_id_nano(self):
        """Test loading Nano from built-in defaults."""
        config = BoardConfig.from_board_id("nano")
        assert config.name == "Arduino Nano"
        assert config.mcu == "atmega328p"
        assert config.variant == "eightanaloginputs"

    def test_from_board_id_leonardo(self):
        """Test loading Leonardo from built-in defaults."""
        config = BoardConfig.from_board_id("leonardo")
        assert config.name == "Arduino Leonardo"
        assert config.mcu == "atmega32u4"
        assert config.variant == "leonardo"

    def test_from_board_id_unknown(self):
        """Test loading unknown board."""
        with pytest.raises(BoardConfigError, match="Unknown board"):
            BoardConfig.from_board_id("unknown")

    def test_from_board_id_with_overrides(self):
        """Test loading with overrides."""
        overrides = {"f_cpu": "8000000L"}
        config = BoardConfig.from_board_id("uno", overrides)
        assert config.f_cpu == "8000000L"
        assert config.mcu == "atmega328p"  # Not overridden

    def test_from_board_id_mega_variant(self):
        """Test that megaatmega2560 is normalized to mega."""
        config = BoardConfig.from_board_id("megaatmega2560")
        assert config.name == "Arduino Mega"
        assert config.mcu == "atmega2560"

    # Test _parse_boards_txt
    def test_parse_boards_txt_basic(self, minimal_boards_txt):
        """Test parsing basic boards.txt."""
        data = BoardConfig._parse_boards_txt(minimal_boards_txt, "uno")
        assert data["name"] == "Arduino Uno"
        assert data["mcu"] == "atmega328p"
        assert data["f_cpu"] == "16000000L"
        assert data["board"] == "AVR_UNO"

    def test_parse_boards_txt_with_comments(self, tmp_boards_txt):
        """Test that comments are ignored."""
        content = """
# This is a comment
uno.name=Arduino Uno
# Another comment
uno.build.mcu=atmega328p
uno.build.f_cpu=16000000L
uno.build.board=AVR_UNO
"""
        tmp_boards_txt.write_text(content)
        data = BoardConfig._parse_boards_txt(tmp_boards_txt, "uno")
        assert "name" in data
        assert data["mcu"] == "atmega328p"

    def test_parse_boards_txt_empty_lines(self, tmp_boards_txt):
        """Test that empty lines are ignored."""
        content = """
uno.name=Arduino Uno

uno.build.mcu=atmega328p

uno.build.f_cpu=16000000L
uno.build.board=AVR_UNO
"""
        tmp_boards_txt.write_text(content)
        data = BoardConfig._parse_boards_txt(tmp_boards_txt, "uno")
        assert len(data) == 4

    def test_parse_boards_txt_ignores_other_boards(self, multi_board_boards_txt):
        """Test that only requested board is parsed."""
        data = BoardConfig._parse_boards_txt(multi_board_boards_txt, "uno")
        assert data["name"] == "Arduino Uno"
        # Check we got Uno data, not Mega data
        assert data["variant"] == "standard"  # Uno uses standard, Mega uses mega

    # Test get_defines
    def test_get_defines_basic(self):
        """Test getting preprocessor defines."""
        config = BoardConfig(
            name="Arduino Uno", mcu="atmega328p", f_cpu="16000000L", board="AVR_UNO"
        )
        defines = config.get_defines()
        assert defines["F_CPU"] == "16000000L"
        assert defines["ARDUINO"] == "10819"
        assert "ARDUINO_AVR_UNO" in defines
        assert "ARDUINO_ARCH_AVR" in defines

    def test_get_defines_with_extra_flags(self):
        """Test defines with extra flags."""
        config = BoardConfig(
            name="Test Board",
            mcu="atmega328p",
            f_cpu="16000000L",
            board="AVR_TEST",
            extra_flags={"define_custom": "1"},
        )
        defines = config.get_defines()
        assert "CUSTOM" in defines
        assert defines["CUSTOM"] == "1"

    # Test get_include_paths
    def test_get_include_paths_default(self):
        """Test getting include paths."""
        config = BoardConfig(
            name="Arduino Uno",
            mcu="atmega328p",
            f_cpu="16000000L",
            board="AVR_UNO",
            core="arduino",
            variant="standard",
        )
        core_path = Path("/arduino/hardware/avr")
        includes = config.get_include_paths(core_path)
        assert len(includes) == 2
        assert includes[0] == Path("/arduino/hardware/avr/cores/arduino")
        assert includes[1] == Path("/arduino/hardware/avr/variants/standard")

    def test_get_include_paths_custom_variant(self):
        """Test include paths with custom variant."""
        config = BoardConfig(
            name="Arduino Mega",
            mcu="atmega2560",
            f_cpu="16000000L",
            board="AVR_MEGA2560",
            variant="mega",
        )
        core_path = Path("/arduino/hardware/avr")
        includes = config.get_include_paths(core_path)
        assert includes[1] == Path("/arduino/hardware/avr/variants/mega")

    # Test get_core_sources_dir
    def test_get_core_sources_dir(self):
        """Test getting core sources directory."""
        config = BoardConfig(
            name="Arduino Uno",
            mcu="atmega328p",
            f_cpu="16000000L",
            board="AVR_UNO",
            core="arduino",
        )
        core_path = Path("/arduino/hardware/avr")
        sources_dir = config.get_core_sources_dir(core_path)
        assert sources_dir == Path("/arduino/hardware/avr/cores/arduino")

    # Test get_variant_dir
    def test_get_variant_dir(self):
        """Test getting variant directory."""
        config = BoardConfig(
            name="Arduino Uno",
            mcu="atmega328p",
            f_cpu="16000000L",
            board="AVR_UNO",
            variant="standard",
        )
        core_path = Path("/arduino/hardware/avr")
        variant_dir = config.get_variant_dir(core_path)
        assert variant_dir == Path("/arduino/hardware/avr/variants/standard")

    # Test __repr__
    def test_repr(self):
        """Test string representation."""
        config = BoardConfig(
            name="Arduino Uno", mcu="atmega328p", f_cpu="16000000L", board="AVR_UNO"
        )
        repr_str = repr(config)
        assert "Arduino Uno" in repr_str
        assert "atmega328p" in repr_str
        assert "16000000L" in repr_str
        assert "AVR_UNO" in repr_str

    # Integration tests
    def test_real_world_uno_config(self):
        """Test complete Uno configuration flow."""
        config = BoardConfig.from_board_id("uno")

        # Check basic properties
        assert config.mcu == "atmega328p"
        assert config.f_cpu == "16000000L"

        # Check defines
        defines = config.get_defines()
        assert defines["F_CPU"] == "16000000L"
        assert "ARDUINO_AVR_UNO" in defines

        # Check paths
        core_path = Path("/arduino")
        includes = config.get_include_paths(core_path)
        assert len(includes) == 2
        assert "cores" in str(includes[0])
        assert "variants" in str(includes[1])

    def test_real_world_mega_config(self):
        """Test complete Mega configuration flow."""
        config = BoardConfig.from_board_id("mega")

        # Check basic properties
        assert config.mcu == "atmega2560"
        assert config.variant == "mega"

        # Check defines
        defines = config.get_defines()
        assert "ARDUINO_AVR_MEGA2560" in defines

    def test_override_frequency_for_3v3(self):
        """Test overriding frequency for 3.3V operation."""
        overrides = {"f_cpu": "8000000L"}
        config = BoardConfig.from_board_id("uno", overrides)

        assert config.f_cpu == "8000000L"
        defines = config.get_defines()
        assert defines["F_CPU"] == "8000000L"
