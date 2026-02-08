"""ESP8266 Framework Management.

This module handles downloading, extracting, and managing the Arduino-ESP8266 framework
needed for ESP8266 builds.

Framework Structure (after extraction):
    framework-arduinoespressif8266/
    ├── cores/
    │   └── esp8266/            # Arduino core implementation
    │       ├── Arduino.h
    │       ├── main.cpp
    │       ├── core_esp8266_wiring.c
    │       └── ...
    ├── variants/
    │   └── nodemcu/            # Board-specific variant
    │       ├── pins_arduino.h
    │       └── ...
    ├── libraries/              # Built-in libraries (ESP8266WiFi, Wire, SPI, etc.)
    │   ├── ESP8266WiFi/
    │   ├── Wire/
    │   ├── SPI/
    │   └── ...
    └── tools/
        └── sdk/                # ESP8266 SDK
            ├── include/        # SDK headers
            └── lib/            # Precompiled libraries
"""

from pathlib import Path
from typing import Any, Dict, List

from .cache import Cache
from .downloader import DownloadError, ExtractionError, PackageDownloader
from .package import IFramework, PackageError


class FrameworkErrorESP8266(PackageError):
    """Raised when ESP8266 framework operations fail."""

    pass


class FrameworkESP8266(IFramework):
    """Manages ESP8266 framework download, extraction, and access.

    This class handles the Arduino-ESP8266 framework which includes:
    - Arduino core for ESP8266 (cores/, variants/)
    - Built-in Arduino libraries (ESP8266WiFi, Wire, SPI, etc.)
    - ESP8266 SDK libraries and headers
    """

    def __init__(
        self,
        cache: Cache,
        framework_url: str,
        show_progress: bool = True,
    ):
        """Initialize ESP8266 framework manager.

        Args:
            cache: Cache manager instance
            framework_url: URL to Arduino-ESP8266 core package
            show_progress: Whether to show download/extraction progress
        """
        self.cache = cache
        self.framework_url = framework_url
        self.show_progress = show_progress
        self.downloader = PackageDownloader()

        # Extract version from URL
        self.version = self._extract_version_from_url(framework_url)

        # Framework path will be determined after download
        self._framework_path: Path | None = None

    @staticmethod
    def _extract_version_from_url(url: str) -> str:
        """Extract version string from framework URL.

        Args:
            url: Framework URL

        Returns:
            Version string
        """
        # Try to extract version from URL
        parts = url.split("/")
        for i, part in enumerate(parts):
            if part == "download" and i + 1 < len(parts):
                return parts[i + 1].lstrip("v")

        # Fallback: use URL hash
        from .cache import Cache

        return Cache.hash_url(url)[:8]

    def ensure_package(self) -> Path:
        """Ensure framework is downloaded and extracted.

        Returns:
            Path to the extracted framework directory

        Raises:
            FrameworkErrorESP8266: If download or extraction fails
        """
        return self.ensure_framework()

    def ensure_framework(self) -> Path:
        """Ensure ESP8266 framework is downloaded and extracted.

        Returns:
            Path to the extracted framework directory

        Raises:
            FrameworkErrorESP8266: If download or extraction fails
        """
        if self._framework_path and self._framework_path.exists():
            return self._framework_path

        try:
            # Get cache path for this framework (use get_platform_path like ESP32)
            framework_cache_path = self.cache.get_platform_path(self.framework_url, self.version)

            if not framework_cache_path.exists():
                # Download and extract framework
                cache_dir = framework_cache_path.parent
                self.downloader.download_and_extract(
                    url=self.framework_url,
                    cache_dir=cache_dir,
                    extract_dir=framework_cache_path,
                    show_progress=self.show_progress,
                )

            self._framework_path = framework_cache_path
            return framework_cache_path

        except (DownloadError, ExtractionError) as e:
            raise FrameworkErrorESP8266(f"Failed to setup ESP8266 framework: {e}") from e

    def get_framework_path(self) -> Path:
        """Get the path to the extracted framework directory.

        Returns:
            Path to framework directory

        Raises:
            FrameworkErrorESP8266: If framework not yet downloaded
        """
        if not self._framework_path:
            raise FrameworkErrorESP8266("Framework not initialized - call ensure_framework() first")

        return self._framework_path

    def get_cores_dir(self) -> Path:
        """Get the path to the cores/esp8266 directory.

        Returns:
            Path to cores directory

        Raises:
            FrameworkErrorESP8266: If cores directory not found
        """
        framework_path = self.get_framework_path()
        cores_dir = framework_path / "cores" / "esp8266"

        if not cores_dir.exists():
            raise FrameworkErrorESP8266(f"Cores directory not found: {cores_dir}")

        return cores_dir

    def get_core_dir(self, core_name: str) -> Path:
        """Get path to specific core directory.

        Args:
            core_name: Core name (e.g., "esp8266")

        Returns:
            Path to the core directory

        Raises:
            FrameworkErrorESP8266: If core directory doesn't exist
        """
        framework_path = self.get_framework_path()
        core_path = framework_path / "cores" / core_name
        if not core_path.exists():
            raise FrameworkErrorESP8266(f"Core '{core_name}' not found at {core_path}")
        return core_path

    def get_core_sources(self, core_name: str) -> List[Path]:
        """Get all source files in a core.

        Args:
            core_name: Core name (e.g., "esp8266")

        Returns:
            List of .c and .cpp source file paths
        """
        core_dir = self.get_core_dir(core_name)
        sources: List[Path] = []
        sources.extend(core_dir.glob("*.c"))
        sources.extend(core_dir.glob("*.cpp"))
        # Also search in subdirectories
        sources.extend(core_dir.glob("**/*.c"))
        sources.extend(core_dir.glob("**/*.cpp"))
        # Remove duplicates
        return list(set(sources))

    def get_variants_dir(self, variant: str) -> Path:
        """Get the path to a specific variant directory.

        Args:
            variant: Variant name (e.g., "nodemcu")

        Returns:
            Path to variant directory

        Raises:
            FrameworkErrorESP8266: If variant directory not found
        """
        framework_path = self.get_framework_path()
        variant_dir = framework_path / "variants" / variant

        if not variant_dir.exists():
            raise FrameworkErrorESP8266(f"Variant directory not found: {variant_dir}")

        return variant_dir

    def get_variant_dir(self, variant: str) -> Path:
        """Get the path to a specific variant directory.

        Alias for get_variants_dir() - matches the interface expected by
        configurable_compiler.py.

        Args:
            variant: Variant name (e.g., "nodemcu")

        Returns:
            Path to variant directory
        """
        return self.get_variants_dir(variant)

    def get_libraries_dir(self) -> Path:
        """Get the path to the libraries directory.

        Returns:
            Path to libraries directory

        Raises:
            FrameworkErrorESP8266: If libraries directory not found
        """
        framework_path = self.get_framework_path()
        libraries_dir = framework_path / "libraries"

        if not libraries_dir.exists():
            raise FrameworkErrorESP8266(f"Libraries directory not found: {libraries_dir}")

        return libraries_dir

    def get_sdk_include_dirs(self) -> List[Path]:
        """Get list of SDK include directories.

        Returns:
            List of include directory paths

        Raises:
            FrameworkErrorESP8266: If SDK directories not found
        """
        framework_path = self.get_framework_path()
        include_dirs: List[Path] = []

        sdk_base = framework_path / "tools" / "sdk"

        # Add tools/sdk/include directory (c_types.h, ets_sys.h, etc.)
        sdk_include = sdk_base / "include"
        if sdk_include.exists():
            include_dirs.append(sdk_include)

        # Add tools/sdk/lwip2/include (network stack headers)
        lwip2_include = sdk_base / "lwip2" / "include"
        if lwip2_include.exists():
            include_dirs.append(lwip2_include)

        # Add tools/sdk/libb64/include
        libb64_include = sdk_base / "libb64" / "include"
        if libb64_include.exists():
            include_dirs.append(libb64_include)

        return include_dirs

    def get_sdk_includes(self, mcu: str) -> List[Path]:
        """Get SDK include directories (interface compatible with configurable_compiler).

        Args:
            mcu: MCU name (unused for ESP8266, present for interface compatibility)

        Returns:
            List of SDK include directory paths
        """
        return self.get_sdk_include_dirs()

    def get_linker_script_dir(self) -> Path:
        """Get the path to the linker script directory.

        Returns:
            Path to the directory containing .ld linker scripts

        Raises:
            FrameworkErrorESP8266: If linker script directory not found
        """
        framework_path = self.get_framework_path()
        ld_dir = framework_path / "tools" / "sdk" / "ld"

        if not ld_dir.exists():
            raise FrameworkErrorESP8266(f"Linker script directory not found: {ld_dir}")

        return ld_dir

    def get_sdk_lib_dirs(self) -> List[Path]:
        """Get all SDK library directories.

        ESP8266 SDK libraries are split across multiple directories:
        - tools/sdk/lib/ - common libraries (libbearssl, libhal, liblwip2, libstdc++)
        - tools/sdk/lib/NONOSDK305/ - NONOS SDK 3.0.5 specific libraries (libphy, libpp, etc.)

        Returns:
            List of SDK library directory paths

        Raises:
            FrameworkErrorESP8266: If base SDK lib directory not found
        """
        framework_path = self.get_framework_path()
        sdk_lib_base = framework_path / "tools" / "sdk" / "lib"

        if not sdk_lib_base.exists():
            raise FrameworkErrorESP8266(f"SDK lib directory not found: {sdk_lib_base}")

        dirs = [sdk_lib_base]

        # Add NONOSDK305 subdirectory (default SDK for ESP8266 Arduino 3.x)
        nonosdk_dir = sdk_lib_base / "NONOSDK305"
        if nonosdk_dir.exists():
            dirs.append(nonosdk_dir)

        return dirs

    def is_installed(self) -> bool:
        """Check if framework is already installed.

        Returns:
            True if framework directory exists with key directories
        """
        if not self._framework_path or not self._framework_path.exists():
            return False

        # Verify essential framework directories exist
        required_dirs = [
            self._framework_path / "cores" / "esp8266",
            self._framework_path / "variants",
            self._framework_path / "libraries",
        ]

        return all(d.exists() for d in required_dirs)

    def get_package_info(self) -> Dict[str, Any]:
        """Get information about the installed framework.

        Returns:
            Dictionary with framework information
        """
        return {
            "type": "framework",
            "name": "arduino-esp8266",
            "version": self.version,
            "path": str(self._framework_path) if self._framework_path else None,
            "url": self.framework_url,
        }
