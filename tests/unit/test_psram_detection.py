"""
Unit tests for ESP32 PSRAM detection and flag generation.

Tests the OrchestratorESP32.board_has_psram() method and _add_psram_flags()
to ensure correct build flags are generated for boards with and without PSRAM.
"""

import pytest
from fbuild.build.orchestrator_esp32 import OrchestratorESP32, NO_PSRAM_BOARDS
from fbuild.packages import Cache


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


def test_add_psram_flags_for_xiao_no_psram():
    """Verify correct flags are generated for XIAO ESP32-S3 (no PSRAM)."""
    cache = Cache()
    orch = OrchestratorESP32(cache, verbose=False)

    # Start with empty build flags
    build_flags = []

    # Add PSRAM flags for XIAO ESP32-S3 (no PSRAM)
    result_flags = orch._add_psram_flags(
        board_id="seeed_xiao_esp32s3",
        mcu="esp32s3",
        build_flags=build_flags,
        verbose=False
    )

    # Should add cache config flag for no-PSRAM board
    assert "-DCONFIG_ESP32S3_DATA_CACHE_64KB" in result_flags

    # Should NOT add PSRAM flags
    assert "-DBOARD_HAS_PSRAM" not in result_flags
    assert "-DCONFIG_SPIRAM_USE_MALLOC" not in result_flags


def test_add_psram_flags_for_esp32dev_with_psram():
    """Verify correct flags are generated for standard ESP32 dev board (with PSRAM)."""
    cache = Cache()
    orch = OrchestratorESP32(cache, verbose=False)

    # Start with empty build flags
    build_flags = []

    # Add PSRAM flags for esp32dev (has PSRAM)
    result_flags = orch._add_psram_flags(
        board_id="esp32dev",
        mcu="esp32s3",
        build_flags=build_flags,
        verbose=False
    )

    # Should add PSRAM enable flags
    assert "-DBOARD_HAS_PSRAM" in result_flags
    assert "-DCONFIG_SPIRAM_USE_MALLOC" in result_flags

    # Should NOT add cache config flag (that's for no-PSRAM boards)
    assert "-DCONFIG_ESP32S3_DATA_CACHE_64KB" not in result_flags


def test_add_psram_flags_removes_dangerous_flags():
    """Verify dangerous PSRAM flags are removed from no-PSRAM boards."""
    cache = Cache()
    orch = OrchestratorESP32(cache, verbose=False)

    # Start with dangerous flags already present (simulating user error)
    build_flags = [
        "-DBOARD_HAS_PSRAM",
        "-DCONFIG_SPIRAM_USE_MALLOC",
        "-DSOME_OTHER_FLAG"
    ]

    # Add PSRAM flags for XIAO ESP32-S3 (no PSRAM)
    result_flags = orch._add_psram_flags(
        board_id="seeed_xiao_esp32s3",
        mcu="esp32s3",
        build_flags=build_flags,
        verbose=False
    )

    # Dangerous flags should be removed
    assert "-DBOARD_HAS_PSRAM" not in result_flags
    assert "-DCONFIG_SPIRAM_USE_MALLOC" not in result_flags

    # Safe cache config should be added
    assert "-DCONFIG_ESP32S3_DATA_CACHE_64KB" in result_flags

    # Other flags should remain
    assert "-DSOME_OTHER_FLAG" in result_flags


def test_add_psram_flags_only_applies_to_esp32s3():
    """Verify PSRAM logic only applies to ESP32-S3 MCU."""
    cache = Cache()
    orch = OrchestratorESP32(cache, verbose=False)

    # Test with ESP32 (not S3)
    build_flags = []
    result_flags = orch._add_psram_flags(
        board_id="seeed_xiao_esp32s3",
        mcu="esp32",  # Different MCU
        build_flags=build_flags,
        verbose=False
    )

    # Should not add any flags for non-S3 MCU
    assert "-DCONFIG_ESP32S3_DATA_CACHE_64KB" not in result_flags
    assert "-DBOARD_HAS_PSRAM" not in result_flags
    assert result_flags == []


def test_add_psram_flags_doesnt_duplicate():
    """Verify flags are not duplicated if already present."""
    cache = Cache()
    orch = OrchestratorESP32(cache, verbose=False)

    # Start with cache config flag already present
    build_flags = ["-DCONFIG_ESP32S3_DATA_CACHE_64KB"]

    result_flags = orch._add_psram_flags(
        board_id="seeed_xiao_esp32s3",
        mcu="esp32s3",
        build_flags=build_flags,
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
        board_id="seeed_xiao_esp32s3",
        mcu="esp32s3",
        build_flags=original_flags,
        verbose=False
    )

    # Original flags should be unchanged
    assert original_flags == original_copy

    # Result should be different
    assert result_flags != original_flags
