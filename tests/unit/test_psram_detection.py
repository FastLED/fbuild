"""
Unit tests for ESP32 PSRAM detection and flag generation.

Tests the OrchestratorESP32._add_psram_flags() to ensure correct build flags
are generated for boards with and without PSRAM.

IMPORTANT: The authoritative source for PSRAM detection is the board JSON's
extra_flags field. If -DBOARD_HAS_PSRAM is present in extra_flags, the board
has PSRAM. If not, it doesn't.
"""

from fbuild.build.orchestrator_esp32 import OrchestratorESP32
from fbuild.build.psram_utils import NO_PSRAM_BOARDS
from fbuild.packages import Cache


# Mock board JSON for boards WITHOUT PSRAM (like esp32-s3-devkitc-1 N8)
BOARD_JSON_NO_PSRAM = {
    "build": {
        "mcu": "esp32s3",
        "extra_flags": [
            "-DARDUINO_ESP32S3_DEV",
            "-DARDUINO_USB_MODE=1"
        ],
        "arduino": {
            "partitions": "default_8MB.csv"
        }
    },
    "name": "Espressif ESP32-S3-DevKitC-1-N8 (8 MB QD, No PSRAM)"
}

# Mock board JSON for boards WITH PSRAM (like adafruit_feather_esp32s2)
BOARD_JSON_WITH_PSRAM = {
    "build": {
        "mcu": "esp32s3",
        "extra_flags": [
            "-DARDUINO_ADAFRUIT_FEATHER_ESP32S3",
            "-DBOARD_HAS_PSRAM",
            "-DARDUINO_USB_CDC_ON_BOOT=1"
        ],
        "arduino": {
            "partitions": "default_8MB.csv"
        }
    },
    "name": "Adafruit Feather ESP32-S3"
}

# Mock board JSON for non-ESP32S3 boards
BOARD_JSON_NON_S3 = {
    "build": {
        "mcu": "esp32",
        "extra_flags": ["-DARDUINO_ESP32_DEV"]
    }
}


def test_no_psram_boards_list_exists():
    """Verify NO_PSRAM_BOARDS module-level constant exists."""
    assert NO_PSRAM_BOARDS is not None
    assert isinstance(NO_PSRAM_BOARDS, list)
    assert len(NO_PSRAM_BOARDS) > 0


def test_xiao_esp32s3_in_no_psram_list():
    """Verify Seeed XIAO ESP32-S3 is in the NO_PSRAM_BOARDS list."""
    assert "seeed_xiao_esp32s3" in NO_PSRAM_BOARDS


def test_board_has_psram_static_method():
    """Verify board_has_psram() is a static method and callable."""
    assert hasattr(OrchestratorESP32, "board_has_psram")
    assert callable(OrchestratorESP32.board_has_psram)


def test_xiao_esp32s3_has_no_psram():
    """Verify XIAO ESP32-S3 is detected as having no PSRAM."""
    assert OrchestratorESP32.board_has_psram("seeed_xiao_esp32s3") is False


def test_xiao_esp32s3_case_insensitive():
    """Verify PSRAM detection is case-insensitive."""
    assert OrchestratorESP32.board_has_psram("SEEED_XIAO_ESP32S3") is False
    assert OrchestratorESP32.board_has_psram("Seeed_XIAO_Esp32S3") is False


def test_esp32dev_has_psram():
    """Verify standard ESP32 dev board is detected as having PSRAM."""
    # esp32dev is not in NO_PSRAM_BOARDS, so should return True
    assert OrchestratorESP32.board_has_psram("esp32dev") is True


def test_add_psram_flags_method_exists():
    """Verify _add_psram_flags() method exists on orchestrator."""
    # Create a minimal cache object (doesn't need to be functional for this test)
    cache = Cache()
    orch = OrchestratorESP32(cache, verbose=False)

    assert hasattr(orch, "_add_psram_flags")
    assert callable(orch._add_psram_flags)


def test_add_psram_flags_for_no_psram_board():
    """Verify correct flags are generated for boards without PSRAM in extra_flags."""
    cache = Cache()
    orch = OrchestratorESP32(cache, verbose=False)

    # Start with empty build flags
    build_flags = []

    # Add PSRAM flags for a board without -DBOARD_HAS_PSRAM in extra_flags
    result_flags = orch._add_psram_flags(
        board_id="esp32-s3-devkitc-1",
        mcu="esp32s3",
        build_flags=build_flags,
        board_json=BOARD_JSON_NO_PSRAM,
        verbose=False
    )

    # Should add cache config flag for no-PSRAM board
    assert "-DCONFIG_ESP32S3_DATA_CACHE_64KB" in result_flags

    # Should NOT add PSRAM flags
    assert "-DBOARD_HAS_PSRAM" not in result_flags
    assert "-DCONFIG_SPIRAM_USE_MALLOC" not in result_flags


def test_add_psram_flags_for_psram_board():
    """Verify correct flags are generated for boards with PSRAM in extra_flags."""
    cache = Cache()
    orch = OrchestratorESP32(cache, verbose=False)

    # Start with empty build flags
    build_flags = []

    # Add PSRAM flags for a board with -DBOARD_HAS_PSRAM in extra_flags
    result_flags = orch._add_psram_flags(
        board_id="adafruit_feather_esp32s3",
        mcu="esp32s3",
        build_flags=build_flags,
        board_json=BOARD_JSON_WITH_PSRAM,
        verbose=False
    )

    # Should add PSRAM malloc flag
    assert "-DCONFIG_SPIRAM_USE_MALLOC" in result_flags

    # Should NOT add cache config flag (that's for no-PSRAM boards)
    assert "-DCONFIG_ESP32S3_DATA_CACHE_64KB" not in result_flags


def test_add_psram_flags_user_can_override():
    """Verify user can add BOARD_HAS_PSRAM via platformio.ini build_flags."""
    cache = Cache()
    orch = OrchestratorESP32(cache, verbose=False)

    # User adds BOARD_HAS_PSRAM in their platformio.ini build_flags
    build_flags = ["-DBOARD_HAS_PSRAM"]

    # Board JSON does NOT have PSRAM flag, but user overrides
    result_flags = orch._add_psram_flags(
        board_id="esp32-s3-devkitc-1",
        mcu="esp32s3",
        build_flags=build_flags,
        board_json=BOARD_JSON_NO_PSRAM,
        verbose=False
    )

    # Since user added BOARD_HAS_PSRAM, should add SPIRAM malloc flag
    assert "-DCONFIG_SPIRAM_USE_MALLOC" in result_flags

    # Should NOT add cache config flag (user says they have PSRAM)
    assert "-DCONFIG_ESP32S3_DATA_CACHE_64KB" not in result_flags


def test_add_psram_flags_only_applies_to_esp32s3():
    """Verify PSRAM logic only applies to ESP32-S3 MCU."""
    cache = Cache()
    orch = OrchestratorESP32(cache, verbose=False)

    # Test with ESP32 (not S3)
    build_flags = []
    result_flags = orch._add_psram_flags(
        board_id="esp32dev",
        mcu="esp32",  # Not esp32s3
        build_flags=build_flags,
        board_json=BOARD_JSON_NON_S3,
        verbose=False
    )

    # Should not add any flags for non-S3 MCU
    assert "-DCONFIG_ESP32S3_DATA_CACHE_64KB" not in result_flags
    assert "-DCONFIG_SPIRAM_USE_MALLOC" not in result_flags
    assert result_flags == []


def test_add_psram_flags_doesnt_duplicate():
    """Verify flags are not duplicated if already present."""
    cache = Cache()
    orch = OrchestratorESP32(cache, verbose=False)

    # Start with cache config flag already present (e.g., user added it)
    build_flags = ["-DCONFIG_ESP32S3_DATA_CACHE_64KB"]

    result_flags = orch._add_psram_flags(
        board_id="esp32-s3-devkitc-1",
        mcu="esp32s3",
        build_flags=build_flags,
        board_json=BOARD_JSON_NO_PSRAM,
        verbose=False
    )

    # Should not duplicate the flag
    count = result_flags.count("-DCONFIG_ESP32S3_DATA_CACHE_64KB")
    assert count == 1, f"Flag duplicated! Found {count} instances"


def test_add_psram_flags_immutability():
    """Verify _add_psram_flags doesn't modify the original build_flags list."""
    cache = Cache()
    orch = OrchestratorESP32(cache, verbose=False)

    # Original flags
    original_flags = ["-DSOME_FLAG"]
    original_copy = original_flags.copy()

    # Call _add_psram_flags
    result_flags = orch._add_psram_flags(
        board_id="esp32-s3-devkitc-1",
        mcu="esp32s3",
        build_flags=original_flags,
        board_json=BOARD_JSON_NO_PSRAM,
        verbose=False
    )

    # Original flags should be unchanged
    assert original_flags == original_copy

    # Result should be different
    assert result_flags != original_flags
