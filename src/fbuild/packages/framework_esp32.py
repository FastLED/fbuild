"""ESP32 Framework Management.

This module handles downloading, extracting, and managing the Arduino-ESP32 framework
and ESP-IDF libraries needed for ESP32 builds.

Framework Download Process:
    1. Download Arduino-ESP32 core (framework-arduinoespressif32)
    2. Download ESP-IDF precompiled libraries (framework-arduinoespressif32-libs)
    3. Extract both archives (.tar.xz format)
    4. Provide access to cores/, variants/, libraries/, and tools/

Framework Structure (after extraction):
    framework-arduinoespressif32/
    ├── cores/
    │   └── esp32/              # Arduino core implementation
    │       ├── Arduino.h
    │       ├── main.cpp
    │       ├── wiring.c
    │       └── ...
    ├── variants/
    │   └── esp32c6/            # Board-specific variant
    │       ├── pins_arduino.h
    │       └── ...
    ├── libraries/              # Built-in libraries (Wire, SPI, WiFi, etc.)
    │   ├── WiFi/
    │   ├── Wire/
    │   ├── SPI/
    │   └── ...
    ├── tools/                  # Build tools and SDKs
    │   └── sdk/
    │       └── esp32c6/
    │           ├── include/    # ESP-IDF headers
    │           └── lib/        # Precompiled libraries
    └── package.json            # Framework metadata

ESP-IDF Libraries Structure:
    framework-arduinoespressif32-libs/
    └── esp32c6/
        ├── include/            # ESP-IDF headers
        │   ├── esp_system/
        │   ├── freertos/
        │   └── ...
        └── lib/                # Precompiled .a libraries
            ├── libesp_system.a
            ├── libfreertos.a
            └── ...
"""

import _thread
import json
from pathlib import Path
from typing import Any, Dict, List, Optional

from .archive_utils import ArchiveExtractor, URLVersionExtractor
from .cache import Cache
from .downloader import DownloadError, ExtractionError
from .header_trampoline_cache import HeaderTrampolineCache
from .package import IFramework, PackageError
from .sdk_utils import SDKPathResolver


class FrameworkErrorESP32(PackageError):
    """Raised when ESP32 framework operations fail."""

    pass


class FrameworkESP32(IFramework):
    """Manages ESP32 framework download, extraction, and access.

    This class handles the Arduino-ESP32 framework which includes:
    - Arduino core for ESP32 family (cores/, variants/)
    - Built-in Arduino libraries (WiFi, Wire, SPI, etc.)
    - ESP-IDF precompiled libraries
    - Tools and SDKs for ESP32 development
    """

    def __init__(
        self,
        cache: Cache,
        framework_url: str,
        libs_url: str,
        skeleton_lib_url: Optional[str] = None,
        show_progress: bool = True,
    ):
        """Initialize ESP32 framework manager.

        Args:
            cache: Cache manager instance
            framework_url: URL to Arduino-ESP32 core package (.tar.xz)
            libs_url: URL to ESP-IDF precompiled libraries (.tar.xz)
            skeleton_lib_url: Optional URL to MCU-specific skeleton library
            show_progress: Whether to show download/extraction progress
        """
        self.cache = cache
        self.framework_url = framework_url
        self.libs_url = libs_url
        self.skeleton_lib_url = skeleton_lib_url
        self.show_progress = show_progress
        self.archive_extractor = ArchiveExtractor(show_progress=show_progress)

        # Extract version from URL (e.g., "3.3.4" from release tag)
        self.version = URLVersionExtractor.extract_version_from_url(framework_url, prefix="esp32-")

        # Get framework paths from cache
        # We'll use a combined hash for all URLs to keep them together
        combined_url = f"{framework_url}|{libs_url}|{skeleton_lib_url or ''}"
        self.framework_path = cache.get_platform_path(combined_url, self.version)

    def ensure_package(self) -> Path:
        """Ensure framework is downloaded and extracted.

        Returns:
            Path to the extracted framework directory

        Raises:
            FrameworkErrorESP32: If download or extraction fails
        """
        return self.ensure_framework()

    def ensure_framework(self) -> Path:
        """Ensure framework is downloaded and extracted.

        Returns:
            Path to the extracted framework directory

        Raises:
            FrameworkErrorESP32: If download or extraction fails
        """
        if self.is_installed():
            if self.show_progress:
                print(f"Using cached ESP32 framework {self.version}")
            return self.framework_path

        try:
            if self.show_progress:
                print(f"Downloading ESP32 framework {self.version}...")

            # Download and extract framework package
            self.cache.ensure_directories()

            # Create framework directory
            self.framework_path.mkdir(parents=True, exist_ok=True)

            # Download and extract Arduino core
            self.archive_extractor.download_and_extract(self.framework_url, self.framework_path, "Arduino-ESP32 core")

            # Download and extract ESP-IDF libraries (if URL is not empty)
            if self.libs_url:
                self.archive_extractor.download_and_extract(
                    self.libs_url,
                    self.framework_path / "tools" / "sdk",
                    "ESP-IDF libraries",
                )

            # Download and extract skeleton library if provided
            if self.skeleton_lib_url:
                if self.show_progress:
                    print("Downloading skeleton library...")
                self.archive_extractor.download_and_extract(
                    self.skeleton_lib_url,
                    self.framework_path / "tools" / "sdk",
                    "Skeleton library",
                )

            if self.show_progress:
                print(f"ESP32 framework installed to {self.framework_path}")

            # Post-install: Generate header trampolines for all MCU variants
            # This pre-generates trampoline caches to avoid Windows command-line length issues
            self._post_install_generate_trampolines()

            return self.framework_path

        except (DownloadError, ExtractionError) as e:
            raise FrameworkErrorESP32(f"Failed to install ESP32 framework: {e}")
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise FrameworkErrorESP32(f"Unexpected error installing framework: {e}")

    def _post_install_generate_trampolines(self) -> None:
        """Generate header trampolines for all MCU variants after framework installation.

        This post-install step pre-generates trampoline caches for all ESP32 MCU variants
        found in the SDK. This ensures that Windows builds with sccache don't hit the
        32K command-line length limit due to excessive -I arguments.

        The trampolines are generated once at framework install time rather than at
        every compile time, improving build performance.
        """
        import platform as platform_module

        # Only generate trampolines on Windows
        if platform_module.system() != "Windows":
            if self.show_progress:
                print("[trampolines] Skipping post-install generation (not Windows)")
            return

        try:
            if self.show_progress:
                print("[trampolines] Post-install: Generating header trampoline caches...")

            # Find all MCU variants in the SDK
            sdk_dir = self.get_sdk_dir()
            if not sdk_dir.exists():
                if self.show_progress:
                    print("[trampolines] Warning: SDK directory not found, skipping")
                return

            # Look for MCU-specific directories
            mcu_variants = []
            for item in sdk_dir.iterdir():
                if item.is_dir() and item.name.startswith("esp32"):
                    mcu_variants.append(item.name)

            if not mcu_variants:
                if self.show_progress:
                    print("[trampolines] Warning: No MCU variants found in SDK")
                return

            if self.show_progress:
                print(f"[trampolines] Found {len(mcu_variants)} MCU variant(s): {', '.join(mcu_variants)}")

            # Exclude ESP-IDF headers that use relative paths or #include_next that break trampolines
            from fbuild.packages.trampoline_excludes import get_exclude_patterns

            exclude_patterns = get_exclude_patterns()

            for mcu in mcu_variants:
                try:
                    # Get SDK include paths for this MCU
                    include_paths = self.get_sdk_includes(mcu)

                    if self.show_progress:
                        print(f"[trampolines] Generating cache for {mcu} ({len(include_paths)} include paths)...")

                    # Use new cache location: .fbuild/cache/trampolines/{mcu}/{hash}/
                    self.cache.ensure_directories()
                    trampoline_cache = HeaderTrampolineCache(
                        cache_root=self.cache.trampolines_dir,
                        show_progress=self.show_progress,
                        mcu_variant=mcu,
                        framework_version=self.version,
                        platform_name="esp32",
                    )

                    # Generate trampolines with exclusions (v3.0 unified design)
                    trampoline_cache.generate_trampolines(include_paths, exclude_patterns=exclude_patterns)
                    # Note: The generate_trampolines method now prints its own progress messages

                except KeyboardInterrupt:
                    _thread.interrupt_main()
                    raise
                except Exception as e:
                    if self.show_progress:
                        print(f"[trampolines] Warning: Failed to generate trampolines for {mcu}: {e}")
                    continue

            if self.show_progress:
                print("[trampolines] Post-install generation complete")

        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception as e:
            # Don't fail the entire installation if trampoline generation fails
            if self.show_progress:
                print(f"[trampolines] Warning: Post-install generation failed: {e}")

    def is_installed(self) -> bool:
        """Check if framework is already installed.

        Returns:
            True if framework directory exists with key files
        """
        if not self.framework_path.exists():
            return False

        # Verify essential framework directories exist
        required_paths = [
            self.framework_path / "cores",
            self.framework_path / "variants",
            self.framework_path / "tools" / "sdk",
        ]

        return all(p.exists() for p in required_paths)

    def get_cores_dir(self) -> Path:
        """Get path to cores directory.

        Returns:
            Path to cores directory containing Arduino core implementation
        """
        return self.framework_path / "cores"

    def get_core_dir(self, core_name: str = "esp32") -> Path:
        """Get path to specific core directory.

        Args:
            core_name: Core name (default: "esp32")

        Returns:
            Path to the core directory

        Raises:
            FrameworkErrorESP32: If core directory doesn't exist
        """
        core_path = self.get_cores_dir() / core_name
        if not core_path.exists():
            raise FrameworkErrorESP32(f"Core '{core_name}' not found at {core_path}")
        return core_path

    def get_variants_dir(self) -> Path:
        """Get path to variants directory.

        Returns:
            Path to variants directory containing board-specific configurations
        """
        return self.framework_path / "variants"

    def get_variant_dir(self, variant_name: str) -> Path:
        """Get path to specific variant directory.

        Args:
            variant_name: Variant name (e.g., "esp32c6")

        Returns:
            Path to the variant directory

        Raises:
            FrameworkErrorESP32: If variant directory doesn't exist
        """
        variant_path = self.get_variants_dir() / variant_name
        if not variant_path.exists():
            raise FrameworkErrorESP32(f"Variant '{variant_name}' not found at {variant_path}")
        return variant_path

    def get_libraries_dir(self) -> Path:
        """Get path to built-in libraries directory.

        Returns:
            Path to libraries directory
        """
        return self.framework_path / "libraries"

    def list_libraries(self) -> List[str]:
        """List all available built-in libraries.

        Returns:
            List of library names
        """
        libs_dir = self.get_libraries_dir()
        if not libs_dir.exists():
            return []

        return [d.name for d in libs_dir.iterdir() if d.is_dir() and not d.name.startswith(".")]

    def get_library_dir(self, library_name: str) -> Optional[Path]:
        """Get path to a specific library.

        Args:
            library_name: Library name (e.g., "WiFi", "Wire")

        Returns:
            Path to library directory or None if not found
        """
        lib_path = self.get_libraries_dir() / library_name
        return lib_path if lib_path.exists() else None

    def get_sdk_dir(self) -> Path:
        """Get path to ESP-IDF SDK directory.

        Returns:
            Path to SDK directory containing ESP-IDF headers and libraries
        """
        return self.framework_path / "tools" / "sdk"

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
        sdk_resolver = SDKPathResolver(self.get_sdk_dir(), self.show_progress)
        return sdk_resolver.get_sdk_includes(mcu)

    def get_sdk_libs(self, mcu: str, flash_mode: str = "qio", psram_mode: str = "qspi") -> List[Path]:
        """Get list of ESP-IDF precompiled libraries for a specific MCU.

        Args:
            mcu: MCU type (e.g., "esp32c6", "esp32s3")
            flash_mode: Flash mode (e.g., "qio", "dio") - determines flash library variant
            psram_mode: PSRAM mode (e.g., "qspi", "opi") - determines PSRAM library variant.
                       For boards without PSRAM, use "qspi" to get non-PSRAM SDK libraries.

        Returns:
            List of .a library file paths
        """
        sdk_resolver = SDKPathResolver(self.get_sdk_dir(), self.show_progress)
        return sdk_resolver.get_sdk_libs(mcu, flash_mode, psram_mode)

    def get_sdk_flags_dir(self, mcu: str) -> Path:
        """Get path to SDK flags directory for a specific MCU.

        Args:
            mcu: MCU type (e.g., "esp32c6", "esp32s3")

        Returns:
            Path to flags directory
        """
        sdk_resolver = SDKPathResolver(self.get_sdk_dir(), self.show_progress)
        return sdk_resolver.get_sdk_flags_dir(mcu)

    def get_tools_dir(self) -> Path:
        """Get path to tools directory.

        Returns:
            Path to tools directory
        """
        return self.framework_path / "tools"

    def get_package_json(self) -> Dict[str, Any]:
        """Load and parse package.json metadata.

        Returns:
            Dictionary containing package metadata

        Raises:
            FrameworkErrorESP32: If package.json doesn't exist or is invalid
        """
        package_json_path = self.framework_path / "package.json"

        if not package_json_path.exists():
            raise FrameworkErrorESP32(f"package.json not found at {package_json_path}. " + "Ensure framework is downloaded first.")

        try:
            with open(package_json_path, "r", encoding="utf-8") as f:
                return json.load(f)
        except json.JSONDecodeError as e:
            raise FrameworkErrorESP32(f"Failed to parse package.json: {e}")
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise FrameworkErrorESP32(f"Failed to read package.json: {e}")

    def list_variants(self) -> List[str]:
        """List all available board variants.

        Returns:
            List of variant names
        """
        variants_dir = self.get_variants_dir()
        if not variants_dir.exists():
            return []

        return [d.name for d in variants_dir.iterdir() if d.is_dir() and not d.name.startswith(".")]

    def list_cores(self) -> List[str]:
        """List all available cores.

        Returns:
            List of core names
        """
        cores_dir = self.get_cores_dir()
        if not cores_dir.exists():
            return []

        return [d.name for d in cores_dir.iterdir() if d.is_dir() and not d.name.startswith(".")]

    def get_core_sources(self, core_name: str = "esp32") -> List[Path]:
        """Get all source files in a core.

        Args:
            core_name: Core name (default: "esp32")

        Returns:
            List of .c and .cpp source file paths
        """
        core_dir = self.get_core_dir(core_name)
        sources = []
        sources.extend(core_dir.glob("*.c"))
        sources.extend(core_dir.glob("*.cpp"))
        # Also search in subdirectories
        sources.extend(core_dir.glob("**/*.c"))
        sources.extend(core_dir.glob("**/*.cpp"))
        # Remove duplicates
        return list(set(sources))

    def get_package_info(self) -> Dict[str, Any]:
        """Get information about the installed framework.

        Returns:
            Dictionary with framework information
        """
        return self.get_framework_info()

    def get_framework_info(self) -> Dict[str, Any]:
        """Get information about the installed framework.

        Returns:
            Dictionary with framework information
        """
        info = {
            "version": self.version,
            "path": str(self.framework_path),
            "framework_url": self.framework_url,
            "libs_url": self.libs_url,
            "installed": self.is_installed(),
        }

        if self.is_installed():
            info["cores_dir"] = str(self.get_cores_dir())
            info["variants_dir"] = str(self.get_variants_dir())
            info["libraries_dir"] = str(self.get_libraries_dir())
            info["sdk_dir"] = str(self.get_sdk_dir())
            info["available_cores"] = self.list_cores()
            info["available_variants"] = self.list_variants()
            info["available_libraries"] = self.list_libraries()

            # Get package information
            try:
                package_json = self.get_package_json()
                info["package_version"] = package_json.get("version")
                info["package_name"] = package_json.get("name")
            except FrameworkErrorESP32:
                pass

        return info
