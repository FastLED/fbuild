"""
Unit tests for BLE compilation with header trampolines.

When the trampoline system removes esp_bt.h (because its relative includes
break through the trampoline redirect), it must preserve the original SDK
include path so that user libraries (e.g., FastLED with BLE) can still
find the header via a direct -I flag.

Tests cover:
- _remove_broken_trampolines removes esp_bt.h from cache
- _find_original_paths_for_broken_headers locates the original SDK path
- generate_trampolines returns the original SDK path alongside the trampoline dir
- The fix works for both cache-hit and cache-miss paths
"""

from pathlib import Path
from typing import List

from fbuild.packages.header_trampoline_cache import HeaderTrampolineCache

# =============================================================================
# Helpers
# =============================================================================


def _create_sdk_tree(tmp_path: Path) -> dict[str, Path]:
    """Create a mock SDK include tree with bt paths and other components.

    Mimics the real layout:
        sdk/include/bt/include/esp32c6/include/esp_bt.h
        sdk/include/freertos/include/freertos/FreeRTOS.h
        sdk/include/esp_system/include/esp_system.h

    Returns:
        Dict with keys: bt_include_dir, freertos_include_dir, esp_system_include_dir
    """
    sdk_include = tmp_path / "sdk" / "include"

    # bt component — contains esp_bt.h
    bt_dir = sdk_include / "bt" / "include" / "esp32c6" / "include"
    bt_dir.mkdir(parents=True, exist_ok=True)
    (bt_dir / "esp_bt.h").write_text('#pragma once\n#include "../../../../controller/esp32c6/esp_bt_cfg.h"\n')

    # freertos component
    freertos_dir = sdk_include / "freertos" / "include"
    freertos_dir.mkdir(parents=True, exist_ok=True)
    (freertos_dir / "FreeRTOS.h").write_text("#pragma once\n// FreeRTOS\n")

    # esp_system component
    esp_system_dir = sdk_include / "esp_system" / "include"
    esp_system_dir.mkdir(parents=True, exist_ok=True)
    (esp_system_dir / "esp_system.h").write_text("#pragma once\n// esp_system\n")

    return {
        "bt_include_dir": bt_dir,
        "freertos_include_dir": freertos_dir,
        "esp_system_include_dir": esp_system_dir,
    }


def _make_cache(tmp_path: Path) -> HeaderTrampolineCache:
    """Create a HeaderTrampolineCache with test-local cache root."""
    return HeaderTrampolineCache(
        cache_root=tmp_path / "trampolines",
        show_progress=False,
        mcu_variant="esp32c6",
        framework_version="3.3.5",
        platform_name="esp32",
    )


# =============================================================================
# Test: _remove_broken_trampolines
# =============================================================================


class TestRemoveBrokenTrampolines:
    """Tests for _remove_broken_trampolines()."""

    def test_removes_esp_bt_h_trampoline(self, tmp_path: Path) -> None:
        """esp_bt.h trampoline is removed from the cache directory."""
        cache = _make_cache(tmp_path)
        cache.cache_root.mkdir(parents=True, exist_ok=True)

        esp_bt = cache.cache_root / "esp_bt.h"
        esp_bt.write_text('#pragma once\n#include "/sdk/bt/include/esp32c6/include/esp_bt.h"\n')

        removed = cache._remove_broken_trampolines()

        assert removed == 1
        assert not esp_bt.exists()

    def test_no_op_when_trampoline_absent(self, tmp_path: Path) -> None:
        """Returns 0 when esp_bt.h trampoline does not exist."""
        cache = _make_cache(tmp_path)
        cache.cache_root.mkdir(parents=True, exist_ok=True)

        removed = cache._remove_broken_trampolines()

        assert removed == 0

    def test_preserves_other_trampolines(self, tmp_path: Path) -> None:
        """Non-broken trampolines are untouched."""
        cache = _make_cache(tmp_path)
        cache.cache_root.mkdir(parents=True, exist_ok=True)

        freertos = cache.cache_root / "FreeRTOS.h"
        freertos.write_text('#pragma once\n#include "/sdk/freertos/FreeRTOS.h"\n')

        esp_bt = cache.cache_root / "esp_bt.h"
        esp_bt.write_text('#pragma once\n#include "/sdk/bt/esp_bt.h"\n')

        cache._remove_broken_trampolines()

        assert freertos.exists(), "Non-broken trampoline should be preserved"
        assert not esp_bt.exists(), "Broken trampoline should be removed"


# =============================================================================
# Test: _find_original_paths_for_broken_headers
# =============================================================================


class TestFindOriginalPathsForBrokenHeaders:
    """Tests for _find_original_paths_for_broken_headers()."""

    def test_finds_sdk_path_containing_esp_bt_h(self, tmp_path: Path) -> None:
        """Returns the SDK directory that contains esp_bt.h."""
        dirs = _create_sdk_tree(tmp_path)
        cache = _make_cache(tmp_path)

        include_paths: List[Path] = [
            dirs["freertos_include_dir"],
            dirs["bt_include_dir"],
            dirs["esp_system_include_dir"],
        ]

        preserved = cache._find_original_paths_for_broken_headers(include_paths)

        assert dirs["bt_include_dir"] in preserved
        assert len(preserved) == 1, "Only the path with esp_bt.h should be preserved"

    def test_returns_empty_when_no_broken_header_present(self, tmp_path: Path) -> None:
        """Returns empty list when none of the paths contain broken headers."""
        dirs = _create_sdk_tree(tmp_path)
        cache = _make_cache(tmp_path)

        # Exclude bt path — only freertos and esp_system
        include_paths: List[Path] = [
            dirs["freertos_include_dir"],
            dirs["esp_system_include_dir"],
        ]

        preserved = cache._find_original_paths_for_broken_headers(include_paths)

        assert preserved == []

    def test_first_match_wins(self, tmp_path: Path) -> None:
        """When multiple paths contain esp_bt.h, only the first is returned."""
        dirs = _create_sdk_tree(tmp_path)
        cache = _make_cache(tmp_path)

        # Create a second directory that also contains esp_bt.h
        alt_bt_dir = tmp_path / "alt_sdk" / "bt"
        alt_bt_dir.mkdir(parents=True, exist_ok=True)
        (alt_bt_dir / "esp_bt.h").write_text("#pragma once\n")

        include_paths: List[Path] = [
            dirs["bt_include_dir"],
            alt_bt_dir,
        ]

        preserved = cache._find_original_paths_for_broken_headers(include_paths)

        assert len(preserved) == 1
        assert preserved[0] == dirs["bt_include_dir"], "First match should win (GCC -I precedence)"


# =============================================================================
# Test: generate_trampolines preserves esp_bt.h SDK path
# =============================================================================


class TestGenerateTrampolinesPreservesEspBt:
    """Tests that generate_trampolines() returns the original SDK bt path.

    After the trampoline for esp_bt.h is removed (because its relative
    includes break through trampolines), the original SDK path must appear
    in the returned include list so that BLE compilation succeeds.
    """

    def test_fresh_generation_includes_bt_path(self, tmp_path: Path) -> None:
        """On fresh cache generation, the bt include path is in the result."""
        dirs = _create_sdk_tree(tmp_path)
        cache = _make_cache(tmp_path)

        include_paths: List[Path] = [
            dirs["freertos_include_dir"],
            dirs["bt_include_dir"],
            dirs["esp_system_include_dir"],
        ]

        result = cache.generate_trampolines(include_paths)

        # The unified trampoline directory should be first
        assert result[0] == cache.cache_root

        # The bt path should be preserved (because esp_bt.h trampoline was removed)
        assert dirs["bt_include_dir"] in result, "Original SDK bt path must be in the result so BLE compilation can find esp_bt.h"

        # esp_bt.h should NOT exist in the trampoline cache (it was removed)
        assert not (cache.cache_root / "esp_bt.h").exists(), "esp_bt.h trampoline should be removed (relative includes break through trampolines)"

    def test_cached_generation_includes_bt_path(self, tmp_path: Path) -> None:
        """On cache hit, the bt include path is still in the result."""
        dirs = _create_sdk_tree(tmp_path)
        cache = _make_cache(tmp_path)

        include_paths: List[Path] = [
            dirs["freertos_include_dir"],
            dirs["bt_include_dir"],
            dirs["esp_system_include_dir"],
        ]

        # First call: generates cache
        result1 = cache.generate_trampolines(include_paths)
        assert dirs["bt_include_dir"] in result1

        # Second call: cache hit
        result2 = cache.generate_trampolines(include_paths)
        assert dirs["bt_include_dir"] in result2, "bt path must be preserved on cache hit too"

    def test_other_headers_still_trampolined(self, tmp_path: Path) -> None:
        """Non-broken headers (FreeRTOS.h, esp_system.h) are still in the trampoline cache."""
        dirs = _create_sdk_tree(tmp_path)
        cache = _make_cache(tmp_path)

        include_paths: List[Path] = [
            dirs["freertos_include_dir"],
            dirs["bt_include_dir"],
            dirs["esp_system_include_dir"],
        ]

        cache.generate_trampolines(include_paths)

        # These should have trampolines in the cache
        assert (cache.cache_root / "FreeRTOS.h").exists(), "FreeRTOS.h should have a trampoline"
        assert (cache.cache_root / "esp_system.h").exists(), "esp_system.h should have a trampoline"

    def test_trampoline_result_usable_as_include_paths(self, tmp_path: Path) -> None:
        """The returned paths form a valid set of -I flags for GCC.

        Specifically: a GCC invocation with these -I flags should be able to
        resolve both #include <FreeRTOS.h> (via trampoline) and
        #include <esp_bt.h> (via direct SDK path).
        """
        dirs = _create_sdk_tree(tmp_path)
        cache = _make_cache(tmp_path)

        include_paths: List[Path] = [
            dirs["freertos_include_dir"],
            dirs["bt_include_dir"],
            dirs["esp_system_include_dir"],
        ]

        result = cache.generate_trampolines(include_paths)

        # Check that esp_bt.h is findable through the result paths
        esp_bt_found = False
        freertos_found = False
        for p in result:
            if (p / "esp_bt.h").exists():
                esp_bt_found = True
            if (p / "FreeRTOS.h").exists():
                freertos_found = True

        assert esp_bt_found, "esp_bt.h must be findable through the returned include paths"
        assert freertos_found, "FreeRTOS.h must be findable through the returned include paths"


# =============================================================================
# Test: Regression — BLE compilation would fail without the fix
# =============================================================================


class TestBLERegressionWithoutFix:
    """Regression tests verifying the BLE compilation fix.

    Before the fix, generate_trampolines() returned [trampoline_dir] which
    did NOT contain esp_bt.h (removed by _remove_broken_trampolines).
    The original SDK path was lost, causing 'esp_bt.h: No such file' errors
    when compiling FastLED with BLE support.
    """

    def test_esp_bt_h_not_in_trampoline_dir(self, tmp_path: Path) -> None:
        """esp_bt.h is NOT in the trampoline directory (by design).

        The trampoline for esp_bt.h is removed because it uses relative
        includes that break through the trampoline redirect.
        """
        dirs = _create_sdk_tree(tmp_path)
        cache = _make_cache(tmp_path)

        include_paths: List[Path] = [
            dirs["freertos_include_dir"],
            dirs["bt_include_dir"],
        ]

        cache.generate_trampolines(include_paths)

        assert not (cache.cache_root / "esp_bt.h").exists()

    def test_esp_bt_h_findable_via_preserved_path(self, tmp_path: Path) -> None:
        """esp_bt.h IS findable via the preserved original SDK path.

        This is the core of the fix: the original SDK path is appended
        to the return value so GCC can find esp_bt.h directly.
        """
        dirs = _create_sdk_tree(tmp_path)
        cache = _make_cache(tmp_path)

        include_paths: List[Path] = [
            dirs["freertos_include_dir"],
            dirs["bt_include_dir"],
        ]

        result = cache.generate_trampolines(include_paths)

        # Simulate GCC searching for esp_bt.h through -I paths
        found = any((p / "esp_bt.h").exists() for p in result)
        assert found, "After the fix, esp_bt.h must be findable through the returned include paths. Without the fix, only the trampoline dir is returned and esp_bt.h was removed from it."

    def test_result_has_more_than_just_trampoline_dir(self, tmp_path: Path) -> None:
        """Result contains more than just the trampoline dir when bt paths exist."""
        dirs = _create_sdk_tree(tmp_path)
        cache = _make_cache(tmp_path)

        include_paths: List[Path] = [
            dirs["freertos_include_dir"],
            dirs["bt_include_dir"],
        ]

        result = cache.generate_trampolines(include_paths)

        # Before the fix: result == [trampoline_dir]  (length 1)
        # After the fix:  result == [trampoline_dir, bt_include_dir]  (length 2+)
        assert len(result) > 1, "Result should include the trampoline dir AND the preserved bt SDK path"
