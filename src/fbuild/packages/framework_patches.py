"""Framework Patching System.

This module provides infrastructure for applying patches to downloaded frameworks
to fix known upstream bugs without modifying the cached archives.

Patches are applied once during framework extraction and persist across builds.
They are version-specific and self-documenting with reasons.
"""

from dataclasses import dataclass
from pathlib import Path
from typing import List

from fbuild.output import log_warning


@dataclass(frozen=True)
class FrameworkPatch:
    """Definition of a single file patch.

    Attributes:
        file_path: Relative path from framework root to file to patch
        find_text: Exact text to find (will error if not found or found multiple times)
        replace_text: Text to replace with
        reason: Human-readable explanation of why this patch is needed
        min_version: Minimum framework version to apply patch (inclusive, None = all)
        max_version: Maximum framework version to apply patch (inclusive, None = all)
    """

    file_path: str
    find_text: str
    replace_text: str
    reason: str
    min_version: str | None = None
    max_version: str | None = None


class FrameworkPatchError(Exception):
    """Raised when framework patching fails."""

    pass


def apply_framework_patches(framework_path: Path, patches: List[FrameworkPatch], framework_version: str, show_progress: bool = True) -> None:
    """Apply a list of patches to a framework.

    Args:
        framework_path: Path to extracted framework directory
        patches: List of patches to apply
        framework_version: Version of the framework being patched
        show_progress: Whether to print progress messages

    Raises:
        FrameworkPatchError: If any patch fails to apply
    """
    if not patches:
        return

    applied_count = 0

    for patch in patches:
        # Check version constraints
        if patch.min_version and framework_version < patch.min_version:
            continue
        if patch.max_version and framework_version > patch.max_version:
            continue

        try:
            _apply_single_patch(framework_path, patch, show_progress)
            applied_count += 1
        except FrameworkPatchError as e:
            # Log but don't fail the entire installation
            if show_progress:
                log_warning(f"Failed to apply framework patch to {patch.file_path}: {e}")
            continue

    if show_progress and applied_count > 0:
        log_warning(f"Applied {applied_count} framework patch(es) to fix upstream bugs")


def _apply_single_patch(framework_path: Path, patch: FrameworkPatch, show_progress: bool) -> None:
    """Apply a single patch to a file.

    Args:
        framework_path: Path to framework root
        patch: Patch to apply
        show_progress: Whether to print progress messages

    Raises:
        FrameworkPatchError: If patch cannot be applied
    """
    file_path = framework_path / patch.file_path

    if not file_path.exists():
        raise FrameworkPatchError(f"File not found: {file_path}")

    # Read file content
    try:
        with open(file_path, "r", encoding="utf-8") as f:
            content = f.read()
    except KeyboardInterrupt:
        raise
    except Exception as e:
        raise FrameworkPatchError(f"Failed to read file: {e}")

    # Check if patch is needed
    if patch.replace_text in content:
        # Already patched - skip silently
        return

    # Count occurrences
    find_count = content.count(patch.find_text)

    if find_count == 0:
        raise FrameworkPatchError("Text not found (may be already fixed upstream)")
    elif find_count > 1:
        raise FrameworkPatchError(f"Text found {find_count} times (expected exactly 1)")

    # Apply patch
    patched_content = content.replace(patch.find_text, patch.replace_text)

    # Write back
    try:
        with open(file_path, "w", encoding="utf-8") as f:
            f.write(patched_content)
    except KeyboardInterrupt:
        raise
    except Exception as e:
        raise FrameworkPatchError(f"Failed to write patched file: {e}")

    if show_progress:
        # Extract filename for cleaner warning message
        filename = Path(patch.file_path).name
        log_warning(f"Patching framework header: {filename} - {patch.reason}")


# =============================================================================
# ESP32 Framework Patch Registry
# =============================================================================

ESP32_FRAMEWORK_PATCHES = [
    FrameworkPatch(
        file_path="tools/sdk/esp32c6/include/bt/include/esp32c6/include/esp_bt.h",
        find_text='#include "../../../../controller/esp32c6/esp_bt_cfg.h"',
        replace_text='#include "../../../controller/esp32c6/esp_bt_cfg.h"',
        reason="Fix incorrect relative path in Arduino ESP32 3.3.6 (ESP-IDF v5.5 upstream bug)",
        min_version="3.3.6",
        max_version="3.3.6",
    ),
    FrameworkPatch(
        file_path="tools/sdk/esp32c6/include/bt/include/esp32c6/include/esp_bt.h",
        find_text=".version_num                = efuse_hal_chip_revision(),",
        replace_text=".version_num                = efuse_hal_get_chip_revision(),",
        reason="Fix incorrect function name in Arduino ESP32 3.3.6 (should use efuse_hal_get_chip_revision)",
        min_version="3.3.6",
        max_version="3.3.6",
    ),
    # Add more patches here as needed for other ESP32 variants or versions
]
