"""ESP32 SDK Path Utilities.

This module provides utilities for discovering and managing ESP-IDF SDK paths,
including include directories and precompiled libraries.
"""

from pathlib import Path
from typing import List


class SDKPathResolver:
    """Resolves SDK paths for ESP-IDF frameworks.

    Provides methods for discovering include directories and libraries
    used by ESP-IDF based projects.
    """

    # MCU fallback mappings for platforms that don't have full SDK support
    MCU_FALLBACKS = {
        "esp32c2": "esp32c3",  # ESP32-C2 can use ESP32-C3 SDK (both rv32imc RISC-V)
    }

    def __init__(self, sdk_base_dir: Path, show_progress: bool = True):
        """Initialize SDK path resolver.

        Args:
            sdk_base_dir: Base directory of the SDK (e.g., framework_path/tools/sdk)
            show_progress: Whether to show progress messages
        """
        self.sdk_base_dir = sdk_base_dir
        self.show_progress = show_progress

    def _resolve_mcu(self, mcu: str) -> str:
        """Resolve MCU to actual SDK directory, applying fallback if needed.

        Args:
            mcu: MCU type (e.g., "esp32c2", "esp32c6")

        Returns:
            Resolved MCU type for SDK lookup
        """
        # Check if MCU SDK directory exists
        mcu_dir = self.sdk_base_dir / mcu
        if mcu_dir.exists():
            return mcu

        # Try fallback if available
        if mcu in self.MCU_FALLBACKS:
            fallback_mcu = self.MCU_FALLBACKS[mcu]
            fallback_dir = self.sdk_base_dir / fallback_mcu
            if fallback_dir.exists():
                if self.show_progress:
                    print(
                        f"      Note: Using {fallback_mcu} SDK for {mcu} (compatible)"
                    )
                return fallback_mcu

        # No fallback available, return original
        return mcu

    def get_sdk_includes(self, mcu: str) -> List[Path]:
        """Get list of ESP-IDF include directories for a specific MCU.

        This method reads the SDK's own includes file which lists the exact
        include paths used by ESP-IDF, avoiding C++ stdlib conflicts that
        occur when recursively discovering paths.

        Args:
            mcu: MCU type (e.g., "esp32c6", "esp32s3")

        Returns:
            List of include directory paths (305 paths for esp32c6)
        """
        # Resolve MCU with fallback if needed
        resolved_mcu = self._resolve_mcu(mcu)

        # Read the SDK's includes file
        includes_file = self.get_sdk_flags_dir(resolved_mcu) / "includes"
        if not includes_file.exists():
            # Fallback to recursive discovery if includes file doesn't exist
            return self._get_sdk_includes_recursive(resolved_mcu)

        try:
            # Read includes file (single line with space-separated entries)
            includes_content = includes_file.read_text().strip()

            # Parse the includes: "-iwithprefixbefore path1 -iwithprefixbefore path2 ..."
            # The -iwithprefixbefore flag means to prepend the SDK include directory
            sdk_include_base = self.sdk_base_dir / resolved_mcu / "include"

            includes = []
            parts = includes_content.split()
            i = 0
            while i < len(parts):
                if parts[i] == "-iwithprefixbefore":
                    # Next part is the relative path
                    if i + 1 < len(parts):
                        rel_path = parts[i + 1]
                        abs_path = sdk_include_base / rel_path
                        if abs_path.exists():
                            includes.append(abs_path)
                        i += 2
                    else:
                        i += 1
                else:
                    i += 1

            return includes

        except KeyboardInterrupt as ke:
            from zapio.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            # Fallback to recursive discovery on error
            if self.show_progress:
                print(f"Warning: Failed to parse includes file: {e}")
                print("Falling back to recursive include discovery")
            return self._get_sdk_includes_recursive(mcu)

    def _get_sdk_includes_recursive(self, mcu: str) -> List[Path]:
        """Fallback method: recursively discover include directories.

        This discovers 557 paths for esp32c6 but causes C++ stdlib conflicts.
        Kept as a fallback when the includes file is not available.

        Args:
            mcu: MCU type (e.g., "esp32c6", "esp32s3")

        Returns:
            List of include directory paths
        """
        sdk_mcu_dir = self.sdk_base_dir / mcu / "include"
        if not sdk_mcu_dir.exists():
            return []

        # Recursively find all subdirectories with header files
        # ESP-IDF has a deep nested structure for includes
        includes = []

        def add_includes_recursive(
            directory: Path, max_depth: int = 6, current_depth: int = 0
        ):
            """Recursively add directories that contain header files."""
            if current_depth > max_depth:
                return

            # Add this directory if it contains headers or is named 'include'
            has_headers = any(directory.glob("*.h"))
            if directory.name == "include" or has_headers:
                includes.append(directory)

            # Special handling for parent directories that have subdirs with headers
            # but no headers themselves. Examples:
            # - .../soc/esp32c6/register/ (has soc/ subdir with headers)
            # - .../esp_rom/esp32c6/include/esp32c6/ (has rom/ subdir with headers)
            # Only add if it matches specific patterns to avoid adding too many paths
            is_parent_dir = False
            if not has_headers:
                # Check for specific directory names that are known parent directories
                # Only add 'register' and MCU dirs that are under 'esp_rom' to be conservative
                if directory.name == "register":
                    is_parent_dir = True
                elif directory.name.startswith("esp32"):
                    # Only add MCU directories if they're under esp_rom component
                    if "esp_rom" in str(directory):
                        is_parent_dir = True

                if is_parent_dir:
                    try:
                        # Check if it has immediate subdirs with headers
                        for subdir in directory.iterdir():
                            if subdir.is_dir() and any(subdir.glob("*.h")):
                                includes.append(directory)
                                break
                    except (PermissionError, OSError):
                        pass

            # Recurse into subdirectories
            try:
                for subdir in directory.iterdir():
                    if subdir.is_dir() and not subdir.name.startswith("."):
                        add_includes_recursive(subdir, max_depth, current_depth + 1)
            except (PermissionError, OSError):
                pass

        add_includes_recursive(sdk_mcu_dir)
        return includes

    def get_sdk_libs(self, mcu: str, flash_mode: str = "qio") -> List[Path]:
        """Get list of ESP-IDF precompiled libraries for a specific MCU.

        Args:
            mcu: MCU type (e.g., "esp32c6", "esp32s3")
            flash_mode: Flash mode (e.g., "qio", "dio") - determines flash library variant

        Returns:
            List of .a library file paths
        """
        # Resolve MCU with fallback if needed
        resolved_mcu = self._resolve_mcu(mcu)

        libs = []

        # Get main SDK libraries
        sdk_lib_dir = self.sdk_base_dir / resolved_mcu / "lib"
        if sdk_lib_dir.exists():
            libs.extend(sdk_lib_dir.glob("*.a"))

        # Get flash mode-specific libraries (qio_qspi or dio_qspi)
        # For ESP32-C6: Only libspi_flash.a
        # For ESP32-S3: Multiple libraries including libfreertos.a, libesp_system.a, etc.
        flash_lib_dir = self.sdk_base_dir / resolved_mcu / f"{flash_mode}_qspi"
        if flash_lib_dir.exists():
            # Collect ALL .a libraries from flash mode directory
            # ESP32-S3 has: libfreertos.a, libspi_flash.a, libesp_system.a,
            #               libesp_hw_support.a, libesp_psram.a, libbootloader_support.a
            flash_libs = list(flash_lib_dir.glob("*.a"))
            libs.extend(flash_libs)

        return libs

    def get_sdk_flags_dir(self, mcu: str) -> Path:
        """Get path to SDK flags directory for a specific MCU.

        Args:
            mcu: MCU type (e.g., "esp32c6", "esp32s3")

        Returns:
            Path to flags directory
        """
        # Resolve MCU with fallback if needed
        resolved_mcu = self._resolve_mcu(mcu)
        return self.sdk_base_dir / resolved_mcu / "flags"
