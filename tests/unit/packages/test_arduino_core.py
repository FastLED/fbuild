"""Unit tests for Arduino core management."""

import os
import tempfile
from pathlib import Path

import pytest

from fbuild.packages.arduino_core import ArduinoCore
from fbuild.packages.cache import Cache


class TestArduinoCore:
    """Test cases for ArduinoCore class."""

    def test_init(self):
        """Test initialization."""
        with tempfile.TemporaryDirectory() as temp_dir:
            cache = Cache(Path(temp_dir))
            core = ArduinoCore(cache)

            assert core.cache == cache
            assert core._core_path is None

    def test_verify_core_success(self):
        """Test core verification succeeds with all required items."""
        with tempfile.TemporaryDirectory() as temp_dir:
            cache = Cache(Path(temp_dir))
            core = ArduinoCore(cache)

            # Create core structure with all required components
            core_path = Path(temp_dir) / "core"
            (core_path / "cores" / "arduino").mkdir(parents=True)
            (core_path / "variants" / "standard").mkdir(parents=True)
            (core_path / "boards.txt").write_text("uno.name=Arduino Uno\n")
            (core_path / "platform.txt").touch()

            # Create required headers
            (core_path / "cores" / "arduino" / "Arduino.h").touch()
            (core_path / "cores" / "arduino" / "HardwareSerial.h").touch()
            (core_path / "variants" / "standard" / "pins_arduino.h").touch()

            # Create required source files
            (core_path / "cores" / "arduino" / "main.cpp").touch()
            (core_path / "cores" / "arduino" / "wiring.c").touch()
            (core_path / "cores" / "arduino" / "wiring_digital.c").touch()

            assert core._verify_core(core_path)

    def test_verify_core_missing_cores(self):
        """Test core verification fails when cores directory missing."""
        with tempfile.TemporaryDirectory() as temp_dir:
            cache = Cache(Path(temp_dir))
            core = ArduinoCore(cache)

            core_path = Path(temp_dir) / "core"
            core_path.mkdir()
            (core_path / "variants").mkdir()
            (core_path / "boards.txt").touch()
            (core_path / "platform.txt").touch()

            assert not core._verify_core(core_path)

    def test_verify_core_missing_variants(self):
        """Test core verification fails when variants directory missing."""
        with tempfile.TemporaryDirectory() as temp_dir:
            cache = Cache(Path(temp_dir))
            core = ArduinoCore(cache)

            core_path = Path(temp_dir) / "core"
            (core_path / "cores" / "arduino").mkdir(parents=True)
            (core_path / "boards.txt").touch()
            (core_path / "platform.txt").touch()

            assert not core._verify_core(core_path)

    def test_verify_core_missing_boards_txt(self):
        """Test core verification fails when boards.txt missing."""
        with tempfile.TemporaryDirectory() as temp_dir:
            cache = Cache(Path(temp_dir))
            core = ArduinoCore(cache)

            core_path = Path(temp_dir) / "core"
            (core_path / "cores" / "arduino").mkdir(parents=True)
            (core_path / "variants").mkdir(parents=True)
            (core_path / "platform.txt").touch()

            assert not core._verify_core(core_path)

    def test_verify_core_missing_platform_txt(self):
        """Test core verification fails when platform.txt missing."""
        with tempfile.TemporaryDirectory() as temp_dir:
            cache = Cache(Path(temp_dir))
            core = ArduinoCore(cache)

            core_path = Path(temp_dir) / "core"
            (core_path / "cores" / "arduino").mkdir(parents=True)
            (core_path / "variants").mkdir(parents=True)
            (core_path / "boards.txt").touch()

            assert not core._verify_core(core_path)

    def test_get_boards_txt_not_initialized(self):
        """Test error when getting boards.txt before initialization."""
        # Import at function level to get current class after any module reloads
        import importlib

        arduino_core_module = importlib.import_module("fbuild.packages.arduino_core")
        importlib.reload(arduino_core_module)

        with tempfile.TemporaryDirectory() as temp_dir:
            cache = Cache(Path(temp_dir))
            core = arduino_core_module.ArduinoCore(cache)

            with pytest.raises(arduino_core_module.ArduinoCoreError, match="not initialized"):
                core.get_boards_txt()

    def test_get_boards_txt_success(self):
        """Test getting boards.txt path."""
        with tempfile.TemporaryDirectory() as temp_dir:
            cache = Cache(Path(temp_dir))
            core = ArduinoCore(cache)

            core_path = Path(temp_dir) / "core"
            core_path.mkdir()
            boards_txt = core_path / "boards.txt"
            boards_txt.touch()

            core._core_path = core_path

            result = core.get_boards_txt()
            assert result == boards_txt

    def test_get_boards_txt_not_found(self):
        """Test error when boards.txt doesn't exist."""
        # Import at function level to get current class after any module reloads
        import importlib

        arduino_core_module = importlib.import_module("fbuild.packages.arduino_core")
        importlib.reload(arduino_core_module)

        with tempfile.TemporaryDirectory() as temp_dir:
            cache = Cache(Path(temp_dir))
            core = arduino_core_module.ArduinoCore(cache)

            core_path = Path(temp_dir) / "core"
            core_path.mkdir()
            core._core_path = core_path

            with pytest.raises(arduino_core_module.ArduinoCoreError, match="boards.txt not found"):
                core.get_boards_txt()

    def test_get_platform_txt(self):
        """Test getting platform.txt path."""
        with tempfile.TemporaryDirectory() as temp_dir:
            cache = Cache(Path(temp_dir))
            core = ArduinoCore(cache)

            core_path = Path(temp_dir) / "core"
            core_path.mkdir()
            platform_txt = core_path / "platform.txt"
            platform_txt.touch()

            core._core_path = core_path

            result = core.get_platform_txt()
            assert result == platform_txt

    def test_get_core_dir(self):
        """Test getting core directory path."""
        with tempfile.TemporaryDirectory() as temp_dir:
            cache = Cache(Path(temp_dir))
            core = ArduinoCore(cache)

            core_path = Path(temp_dir) / "core"
            core_dir = core_path / "cores" / "arduino"
            core_dir.mkdir(parents=True)

            core._core_path = core_path

            result = core.get_core_dir()
            assert result == core_dir

    def test_get_core_dir_not_found(self):
        """Test error when core directory doesn't exist."""
        # Import at function level to get current class after any module reloads
        import importlib

        arduino_core_module = importlib.import_module("fbuild.packages.arduino_core")
        importlib.reload(arduino_core_module)

        with tempfile.TemporaryDirectory() as temp_dir:
            cache = Cache(Path(temp_dir))
            core = arduino_core_module.ArduinoCore(cache)

            core_path = Path(temp_dir) / "core"
            core_path.mkdir()
            core._core_path = core_path

            with pytest.raises(arduino_core_module.ArduinoCoreError, match="cores/arduino directory not found"):
                core.get_core_dir()

    def test_get_variant_dir(self):
        """Test getting variant directory path."""
        with tempfile.TemporaryDirectory() as temp_dir:
            cache = Cache(Path(temp_dir))
            core = ArduinoCore(cache)

            core_path = Path(temp_dir) / "core"
            variant_dir = core_path / "variants" / "standard"
            variant_dir.mkdir(parents=True)

            core._core_path = core_path

            result = core.get_variant_dir("standard")
            assert result == variant_dir

    def test_get_variant_dir_not_found(self):
        """Test error when variant doesn't exist."""
        # Import at function level to get current class after any module reloads
        import importlib

        arduino_core_module = importlib.import_module("fbuild.packages.arduino_core")
        importlib.reload(arduino_core_module)

        with tempfile.TemporaryDirectory() as temp_dir:
            cache = Cache(Path(temp_dir))
            core = arduino_core_module.ArduinoCore(cache)

            core_path = Path(temp_dir) / "core"
            (core_path / "variants").mkdir(parents=True)
            core._core_path = core_path

            with pytest.raises(arduino_core_module.ArduinoCoreError, match="Variant 'nonexistent' not found"):
                core.get_variant_dir("nonexistent")

    def test_get_core_sources(self):
        """Test getting core source files."""
        with tempfile.TemporaryDirectory() as temp_dir:
            cache = Cache(Path(temp_dir))
            core = ArduinoCore(cache)

            core_path = Path(temp_dir) / "core"
            core_dir = core_path / "cores" / "arduino"
            core_dir.mkdir(parents=True)

            # Create some source files
            (core_dir / "wiring.c").touch()
            (core_dir / "main.cpp").touch()
            (core_dir / "Arduino.h").touch()  # Should not be included

            core._core_path = core_path

            sources = core.get_core_sources()
            assert len(sources) == 2
            assert any(s.name == "wiring.c" for s in sources)
            assert any(s.name == "main.cpp" for s in sources)
            assert not any(s.name == "Arduino.h" for s in sources)

    def test_get_core_sources_empty(self):
        """Test getting core sources when none exist."""
        with tempfile.TemporaryDirectory() as temp_dir:
            cache = Cache(Path(temp_dir))
            core = ArduinoCore(cache)

            core_path = Path(temp_dir) / "core"
            core_dir = core_path / "cores" / "arduino"
            core_dir.mkdir(parents=True)

            core._core_path = core_path

            sources = core.get_core_sources()
            assert len(sources) == 0

    def test_get_variant_sources(self):
        """Test getting variant source files."""
        with tempfile.TemporaryDirectory() as temp_dir:
            cache = Cache(Path(temp_dir))
            core = ArduinoCore(cache)

            core_path = Path(temp_dir) / "core"
            variant_dir = core_path / "variants" / "standard"
            variant_dir.mkdir(parents=True)

            # Create some source files
            (variant_dir / "pins_arduino.c").touch()
            (variant_dir / "pins_arduino.h").touch()  # Should not be included

            core._core_path = core_path

            sources = core.get_variant_sources("standard")
            assert len(sources) == 1
            assert sources[0].name == "pins_arduino.c"

    def test_ensure_avr_core_cached(self):
        """Test using cached Arduino core."""
        with tempfile.TemporaryDirectory() as temp_dir:
            # Set FBUILD_CACHE_DIR to use temp directory for cache
            old_cache_dir = os.environ.get("FBUILD_CACHE_DIR")
            os.environ["FBUILD_CACHE_DIR"] = temp_dir
            try:
                cache = Cache(Path(temp_dir))
                cache.ensure_directories()
                core = ArduinoCore(cache)

                # Create a fake cached core with URL-based structure
                core_path = cache.get_platform_path(ArduinoCore.AVR_URL, ArduinoCore.AVR_VERSION)

                (core_path / "cores" / "arduino").mkdir(parents=True)
                (core_path / "variants" / "standard").mkdir(parents=True)
                (core_path / "boards.txt").write_text("uno.name=Arduino Uno\n")
                (core_path / "platform.txt").touch()

                # Create required headers
                (core_path / "cores" / "arduino" / "Arduino.h").touch()
                (core_path / "cores" / "arduino" / "HardwareSerial.h").touch()
                (core_path / "variants" / "standard" / "pins_arduino.h").touch()

                # Create required source files
                (core_path / "cores" / "arduino" / "main.cpp").touch()
                (core_path / "cores" / "arduino" / "wiring.c").touch()
                (core_path / "cores" / "arduino" / "wiring_digital.c").touch()

                # Ensure core (should use cache)
                result = core.ensure_avr_core()
                assert result == core_path
                assert core._core_path == core_path
            finally:
                if old_cache_dir is None:
                    del os.environ["FBUILD_CACHE_DIR"]
                else:
                    os.environ["FBUILD_CACHE_DIR"] = old_cache_dir

    def test_ensure_avr_core_reuse_instance(self):
        """Test that ensure_avr_core returns cached instance."""
        with tempfile.TemporaryDirectory() as temp_dir:
            # Set FBUILD_CACHE_DIR to use temp directory for cache
            old_cache_dir = os.environ.get("FBUILD_CACHE_DIR")
            os.environ["FBUILD_CACHE_DIR"] = temp_dir
            try:
                cache = Cache(Path(temp_dir))
                cache.ensure_directories()
                core = ArduinoCore(cache)

                # Create a fake cached core with URL-based structure
                core_path = cache.get_platform_path(ArduinoCore.AVR_URL, ArduinoCore.AVR_VERSION)

                (core_path / "cores" / "arduino").mkdir(parents=True)
                (core_path / "variants" / "standard").mkdir(parents=True)
                (core_path / "boards.txt").write_text("uno.name=Arduino Uno\n")
                (core_path / "platform.txt").touch()

                # Create required headers
                (core_path / "cores" / "arduino" / "Arduino.h").touch()
                (core_path / "cores" / "arduino" / "HardwareSerial.h").touch()
                (core_path / "variants" / "standard" / "pins_arduino.h").touch()

                # Create required source files
                (core_path / "cores" / "arduino" / "main.cpp").touch()
                (core_path / "cores" / "arduino" / "wiring.c").touch()
                (core_path / "cores" / "arduino" / "wiring_digital.c").touch()

                # First call
                result1 = core.ensure_avr_core()

                # Second call should return same path without checking cache
                result2 = core.ensure_avr_core()
                assert result1 == result2
            finally:
                if old_cache_dir is None:
                    del os.environ["FBUILD_CACHE_DIR"]
                else:
                    os.environ["FBUILD_CACHE_DIR"] = old_cache_dir
