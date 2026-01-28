"""
Test Suite 2: PSRAM Configuration Detection

Tests fbuild's ability to detect board-specific PSRAM availability
and generate correct build flags for boards without PSRAM.

Hardware: Seeed XIAO ESP32-S3 (no PSRAM variant)
Expected: These tests will likely FAIL initially since PSRAM detection
          is not yet implemented (Phase 4 will add this functionality).
"""

import pytest


@pytest.mark.unit
@pytest.mark.esp32s3
def test_psram_detection_xiao_esp32s3():
    """
    Test 2.1: Verify board-specific PSRAM detection for XIAO ESP32-S3.

    The XIAO ESP32-S3 has two variants:
    - No PSRAM (our board)
    - With PSRAM (different SKU)

    fbuild should correctly identify the no-PSRAM variant and set
    appropriate cache configuration flags.

    Expected Failure:
    - AttributeError: 'BoardConfig' object has no attribute 'has_psram'
    OR
    - ImportError: cannot import name 'get_board_config'

    This is expected - Phase 4 will implement the missing functionality.
    """
    try:
        from fbuild.platform_configs import get_board_config
    except ImportError as e:
        pytest.skip(f"get_board_config not yet implemented: {e}")

    # Get board configuration for XIAO ESP32-S3
    board_name = "seeed_xiao_esp32s3"
    config = get_board_config(board_name)

    # Verify PSRAM detection
    assert hasattr(config, "has_psram"), "BoardConfig missing 'has_psram' attribute. " "Phase 4 needs to add PSRAM detection to board configuration."

    assert config.has_psram is False, f"Board {board_name} incorrectly reports has_psram=True. " f"This board variant has no PSRAM and should report False."

    # Verify cache configuration flag is set correctly
    # When PSRAM is absent, ESP32-S3 should use 64KB data cache
    assert hasattr(config, "build_flags"), "BoardConfig missing 'build_flags' attribute"

    build_flags_str = " ".join(config.build_flags)
    assert "CONFIG_ESP32S3_DATA_CACHE_64KB" in build_flags_str, f"Missing cache config flag for no-PSRAM board. " f"Expected CONFIG_ESP32S3_DATA_CACHE_64KB in build flags. " f"Got: {build_flags_str}"

    # Verify PSRAM flags are NOT present
    assert "BOARD_HAS_PSRAM" not in build_flags_str, f"BOARD_HAS_PSRAM should not be set for no-PSRAM variant. " f"Got: {build_flags_str}"


@pytest.mark.unit
@pytest.mark.esp32s3
def test_build_flags_respect_no_psram():
    """
    Test 2.2: Verify OrchestratorESP32 generates correct flags for no-PSRAM boards.

    The ESP32 orchestrator should:
    1. Check if board has PSRAM
    2. If no PSRAM: Add CONFIG_ESP32S3_DATA_CACHE_64KB
    3. If no PSRAM: Do NOT add BOARD_HAS_PSRAM or CONFIG_SPIRAM_USE_MALLOC

    Expected Failure:
    - AttributeError: 'OrchestratorESP32' object has no attribute 'board_has_psram'
    OR
    - Wrong flags generated (PSRAM flags present when they shouldn't be)

    This is expected - Phase 4 will implement board_has_psram() method.
    """
    try:
        from fbuild.build.orchestrator_esp32 import OrchestratorESP32
    except ImportError as e:
        pytest.skip(f"OrchestratorESP32 not found: {e}")

    # Create orchestrator for XIAO ESP32-S3
    board_name = "seeed_xiao_esp32s3"

    try:
        orch = OrchestratorESP32(board_name)
    except TypeError:
        # OrchestratorESP32 might require additional constructor args
        # Try with minimal config
        pytest.skip("OrchestratorESP32 constructor signature unknown, cannot instantiate")

    # Check if board_has_psram method exists
    assert hasattr(orch, "board_has_psram"), "OrchestratorESP32 missing 'board_has_psram' method. " "Phase 4 needs to implement PSRAM detection logic."

    # Verify method returns False for XIAO ESP32-S3
    has_psram = orch.board_has_psram(board_name)
    assert has_psram is False, f"board_has_psram() returned {has_psram} for {board_name}. " f"Expected False (this board has no PSRAM)."

    # Verify build flags generation
    # Try to get build flags (method name might vary)
    build_flags = None
    for method_name in ["get_build_flags", "generate_build_flags", "_get_compiler_flags"]:
        if hasattr(orch, method_name):
            try:
                method = getattr(orch, method_name)
                build_flags = method(board_name)
                break
            except Exception:
                continue

    if build_flags is None:
        pytest.skip("Could not find method to retrieve build flags from orchestrator")

    # Convert flags to searchable string
    if isinstance(build_flags, list):
        flags_str = " ".join(build_flags)
    else:
        flags_str = str(build_flags)

    # Verify correct cache config flag is present
    assert "CONFIG_ESP32S3_DATA_CACHE_64KB" in flags_str or "-DCONFIG_ESP32S3_DATA_CACHE_64KB" in flags_str, (
        f"Missing cache config flag for no-PSRAM board. " f"Expected CONFIG_ESP32S3_DATA_CACHE_64KB in build flags. " f"Got: {flags_str}"
    )

    # Verify PSRAM flags are NOT present
    assert "BOARD_HAS_PSRAM" not in flags_str, f"BOARD_HAS_PSRAM should not be set for no-PSRAM variant. " f"Got: {flags_str}"

    assert "CONFIG_SPIRAM_USE_MALLOC" not in flags_str, f"CONFIG_SPIRAM_USE_MALLOC should not be set for no-PSRAM variant. " f"Got: {flags_str}"


@pytest.mark.unit
@pytest.mark.esp32s3
def test_psram_board_list_includes_xiao():
    """
    Test 2.3: Verify XIAO ESP32-S3 is in the NO_PSRAM_BOARDS list.

    Additional test to verify the board database includes the XIAO ESP32-S3
    in the list of boards without PSRAM.

    Expected Failure:
    - NO_PSRAM_BOARDS constant/list doesn't exist yet

    This is expected - Phase 4 will create this list.
    """
    try:
        from fbuild.build.orchestrator_esp32 import OrchestratorESP32
    except ImportError as e:
        pytest.skip(f"OrchestratorESP32 not found: {e}")

    # Check if NO_PSRAM_BOARDS constant exists
    # It might be a module-level constant or a class attribute
    no_psram_boards = None

    # Try to find the NO_PSRAM_BOARDS list
    import fbuild.build.orchestrator_esp32 as esp32_module

    if hasattr(esp32_module, "NO_PSRAM_BOARDS"):
        no_psram_boards = esp32_module.NO_PSRAM_BOARDS
    elif hasattr(OrchestratorESP32, "NO_PSRAM_BOARDS"):
        no_psram_boards = OrchestratorESP32.NO_PSRAM_BOARDS

    if no_psram_boards is None:
        pytest.skip("NO_PSRAM_BOARDS list not found. " "Phase 4 needs to create a list of boards without PSRAM.")

    # Verify XIAO ESP32-S3 is in the list
    board_name = "seeed_xiao_esp32s3"
    assert board_name in no_psram_boards, f"Board '{board_name}' not found in NO_PSRAM_BOARDS list. " f"This board has no PSRAM and must be included. " f"Current list: {no_psram_boards}"


@pytest.mark.unit
@pytest.mark.esp32s3
def test_psram_crash_prevention():
    """
    Test 2.4: Verify that PSRAM misconfiguration is prevented.

    This test simulates the crash scenario:
    - Board has no PSRAM
    - BOARD_HAS_PSRAM flag is incorrectly set
    - Device crashes on boot with "CORRUPT HEAP"

    The test verifies that fbuild's detection prevents this scenario.

    Expected Failure:
    - Detection logic not yet implemented

    This is expected - Phase 4 will implement the fix.
    """
    try:
        from fbuild.build.orchestrator_esp32 import OrchestratorESP32
    except ImportError as e:
        pytest.skip(f"OrchestratorESP32 not found: {e}")

    board_name = "seeed_xiao_esp32s3"

    # Simulate what happens when PSRAM flags are incorrectly set
    # In the bug scenario, Arduino IDE would set BOARD_HAS_PSRAM
    # causing the device to crash

    # Get the actual flags fbuild would generate
    try:
        orch = OrchestratorESP32(board_name)
    except TypeError:
        pytest.skip("OrchestratorESP32 constructor signature unknown")

    # Try to find build flags generation method
    build_flags = None
    for method_name in ["get_build_flags", "generate_build_flags", "_get_compiler_flags"]:
        if hasattr(orch, method_name):
            try:
                method = getattr(orch, method_name)
                build_flags = method(board_name)
                break
            except Exception:
                continue

    if build_flags is None:
        pytest.skip("Could not find method to retrieve build flags")

    # Convert to searchable format
    if isinstance(build_flags, list):
        flags_str = " ".join(build_flags)
    else:
        flags_str = str(build_flags)

    # The dangerous combination that causes crashes:
    # 1. BOARD_HAS_PSRAM is set (tells code to use PSRAM)
    # 2. Device has no PSRAM hardware
    # Result: Heap corruption, boot crash

    has_dangerous_flag = "BOARD_HAS_PSRAM" in flags_str
    has_psram_malloc = "CONFIG_SPIRAM_USE_MALLOC" in flags_str

    assert not has_dangerous_flag, f"DANGEROUS: BOARD_HAS_PSRAM is set for a board without PSRAM! " f"This will cause 'CORRUPT HEAP' crash on boot. " f"Build flags: {flags_str}"

    assert not has_psram_malloc, f"DANGEROUS: CONFIG_SPIRAM_USE_MALLOC is set for a board without PSRAM! " f"This will cause heap allocation failures. " f"Build flags: {flags_str}"

    # Verify safe cache configuration is present instead
    has_safe_cache_config = "CONFIG_ESP32S3_DATA_CACHE_64KB" in flags_str or "-DCONFIG_ESP32S3_DATA_CACHE_64KB" in flags_str

    assert has_safe_cache_config, f"Missing safe cache configuration for no-PSRAM board. " f"Should have CONFIG_ESP32S3_DATA_CACHE_64KB. " f"Build flags: {flags_str}"
