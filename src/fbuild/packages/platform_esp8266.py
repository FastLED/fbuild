"""ESP8266 Platform Package Management.

This module handles downloading, extracting, and managing ESP8266 platform packages
from PlatformIO registry. It provides access to the Arduino-ESP8266 core, toolchains,
and platform-specific tools needed for ESP8266 builds.

Platform Structure (after extraction):
    platform-espressif8266/
    ├── platform.json           # Package metadata with download URLs
    ├── boards/                 # Board definitions (JSON files)
    │   └── nodemcuv2.json
    ├── builder/                # PlatformIO build scripts
    │   └── frameworks/
    │       └── arduino.py
    └── ...                     # Other platform files

Key Packages (from platform.json):
    - framework-arduinoespressif8266: Arduino core for ESP8266
    - toolchain-xtensa: Xtensa LX106 GCC compiler
    - tool-esptool: Upload tool
"""

import json
from pathlib import Path
from typing import Any, Dict

from .cache import Cache
from .downloader import DownloadError, ExtractionError, PackageDownloader
from .package import IPackage, PackageError
from .platform_utils import PlatformDetector


class PlatformErrorESP8266(PackageError):
    """Raised when ESP8266 platform operations fail."""

    pass


class PlatformESP8266(IPackage):
    """Manages ESP8266 platform package download, extraction, and access.

    This class handles the platform-espressif8266 package which contains:
    - Arduino core for ESP8266
    - Toolchains (xtensa-lx106-elf-gcc)
    - Platform tools (esptool, etc.)
    - Board definitions and variants
    """

    def __init__(self, cache: Cache, platform_url: str, show_progress: bool = True):
        """Initialize ESP8266 platform manager.

        Args:
            cache: Cache manager instance
            platform_url: URL to platform package (e.g., GitHub release ZIP)
            show_progress: Whether to show download/extraction progress
        """
        self.cache = cache
        self.platform_url = platform_url
        self.show_progress = show_progress
        self.downloader = PackageDownloader()

        # Extract version from URL
        self.version = self._extract_version_from_url(platform_url)

        # Get platform path from cache
        self.platform_path = cache.get_platform_path(platform_url, self.version)

    @staticmethod
    def _extract_version_from_url(url: str) -> str:
        """Extract version string from platform URL.

        Args:
            url: Platform URL (e.g., https://github.com/.../v4.2.1/platform.zip)

        Returns:
            Version string (e.g., "4.2.1")
        """
        # URL format: .../releases/download/{version}/platform-espressif8266.zip
        parts = url.split("/")
        for i, part in enumerate(parts):
            if part == "download" and i + 1 < len(parts):
                version = parts[i + 1]
                # Remove 'v' prefix if present
                return version.lstrip("v")

        # Fallback: use URL hash if version extraction fails
        from .cache import Cache

        return Cache.hash_url(url)[:8]

    def ensure_package(self) -> Path:
        """Ensure platform is downloaded and extracted.

        Returns:
            Path to the extracted platform directory

        Raises:
            PlatformErrorESP8266: If download or extraction fails
        """
        return self.ensure_platform()

    def ensure_platform(self) -> Path:
        """Ensure ESP8266 platform is downloaded and extracted.

        Returns:
            Path to the extracted platform directory

        Raises:
            PlatformErrorESP8266: If download or extraction fails
        """
        if self.platform_path.exists():
            return self.platform_path

        try:
            # Download and extract platform package
            cache_dir = self.platform_path.parent
            self.downloader.download_and_extract(
                url=self.platform_url,
                cache_dir=cache_dir,
                extract_dir=self.platform_path,
                show_progress=self.show_progress,
            )

            # Create manifest for cache management
            from fbuild.packages.downloader import create_package_manifest

            create_package_manifest(
                install_path=self.platform_path,
                name=f"ESP8266 Platform {self.version}",
                package_type="platforms",
                version=self.version,
                url=self.platform_url,
                metadata={"platform": "esp8266"},
            )

            return self.platform_path

        except (DownloadError, ExtractionError) as e:
            raise PlatformErrorESP8266(f"Failed to setup ESP8266 platform: {e}") from e

    def get_board_json(self, board_id: str) -> Dict[str, Any]:
        """Get board configuration from boards/ directory.

        Args:
            board_id: Board identifier (e.g., "nodemcuv2")

        Returns:
            Board configuration as dictionary

        Raises:
            PlatformErrorESP8266: If board JSON not found or invalid
        """
        # Find board JSON file
        boards_dir = self.platform_path / "boards"
        board_file = boards_dir / f"{board_id}.json"

        if not board_file.exists():
            raise PlatformErrorESP8266(f"Board definition not found: {board_id}")

        try:
            with open(board_file, encoding="utf-8") as f:
                return json.load(f)
        except (OSError, json.JSONDecodeError) as e:
            raise PlatformErrorESP8266(f"Failed to load board JSON: {e}") from e

    # GitHub release asset names per platform (earlephilhower/esp-quick-toolchain)
    _TOOLCHAIN_ASSET_MAP: Dict[str, str] = {
        "win32": "i686-w64-mingw32.xtensa-lx106-elf-c791b74.230224.zip",
        "win64": "x86_64-w64-mingw32.xtensa-lx106-elf-c791b74.230224.zip",
        "linux-amd64": "x86_64-linux-gnu.xtensa-lx106-elf-c791b74.230224.tar.gz",
        "linux-arm64": "aarch64-linux-gnu.xtensa-lx106-elf-c791b74.230224.tar.gz",
        "linux-armhf": "arm-linux-gnueabihf.xtensa-lx106-elf-c791b74.230224.tar.gz",
        "linux-i686": "i686-linux-gnu.xtensa-lx106-elf-c791b74.230224.tar.gz",
        "macos": "x86_64-apple-darwin14.xtensa-lx106-elf-c791b74.230224.tar.gz",
        "macos-arm64": "x86_64-apple-darwin14.xtensa-lx106-elf-c791b74.230224.tar.gz",
    }

    _TOOLCHAIN_RELEASE_TAG = "3.2.0-gcc10.3"
    _TOOLCHAIN_REPO = "earlephilhower/esp-quick-toolchain"

    def get_required_packages(self, mcu: str) -> Dict[str, str]:
        """Get required package URLs for the given MCU.

        Args:
            mcu: MCU name (e.g., "esp8266")

        Returns:
            Dictionary mapping package names to download URLs
        """
        current_platform = PlatformDetector.detect_esp32_platform()
        asset = self._TOOLCHAIN_ASSET_MAP.get(current_platform, self._TOOLCHAIN_ASSET_MAP["linux-amd64"])
        toolchain_url = f"https://github.com/{self._TOOLCHAIN_REPO}/releases/download/{self._TOOLCHAIN_RELEASE_TAG}/{asset}"

        return {
            "framework": "https://github.com/esp8266/Arduino/archive/refs/tags/3.1.2.zip",
            "toolchain": toolchain_url,
        }

    def is_installed(self) -> bool:
        """Check if platform is already installed.

        Returns:
            True if platform directory exists with key files
        """
        if not self.platform_path.exists():
            return False

        # Verify essential platform files exist
        required_files = [
            self.platform_path / "platform.json",
            self.platform_path / "boards",
        ]

        return all(f.exists() for f in required_files)

    def get_package_info(self) -> Dict[str, Any]:
        """Get information about the installed platform.

        Returns:
            Dictionary with platform information
        """
        return {
            "type": "platform",
            "name": "espressif8266",
            "version": self.version,
            "path": str(self.platform_path),
            "url": self.platform_url,
        }
