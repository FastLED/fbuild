"""ESP32 Platform Package Management.

This module handles downloading, extracting, and managing ESP32 platform packages
from GitHub releases. It provides access to the Arduino-ESP32 core, toolchains,
and platform-specific tools needed for ESP32 builds.

Platform Structure (after extraction):
    platform-espressif32/
    ├── platform.json           # Package metadata with download URLs
    ├── boards/                 # Board definitions (JSON files)
    │   └── esp32-c6-devkitm-1.json
    ├── builder/                # PlatformIO build scripts
    │   └── frameworks/
    │       └── arduino.py
    └── ...                     # Other platform files

Key Packages (from platform.json):
    - framework-arduinoespressif32: Arduino core (cores/, variants/)
    - framework-arduinoespressif32-libs: Pre-built ESP-IDF libraries
    - toolchain-riscv32-esp: RISC-V GCC (for C3, C6, H2)
    - toolchain-xtensa-esp-elf: Xtensa GCC (for ESP32, S2, S3)
    - tool-esptoolpy: Upload tool
"""

import json
from pathlib import Path
from typing import Any, Dict, Optional

from .cache import Cache
from .downloader import DownloadError, ExtractionError, PackageDownloader
from .package import IPackage, PackageError


class PlatformErrorESP32(PackageError):
    """Raised when ESP32 platform operations fail."""

    pass


class PlatformESP32(IPackage):
    """Manages ESP32 platform package download, extraction, and access.

    This class handles the pioarduino/platform-espressif32 package which contains:
    - Arduino core for ESP32 family (C3, C6, S2, S3, H2, etc.)
    - Toolchains (riscv32-esp-elf-gcc, xtensa-esp-elf-gcc)
    - Platform tools (esptool, mkspiffs, etc.)
    - Board definitions and variants
    """

    def __init__(self, cache: Cache, platform_url: str, show_progress: bool = True):
        """Initialize ESP32 platform manager.

        Args:
            cache: Cache manager instance
            platform_url: URL to platform package (e.g., GitHub release ZIP)
            show_progress: Whether to show download/extraction progress
        """
        self.cache = cache
        self.platform_url = platform_url
        self.show_progress = show_progress
        self.downloader = PackageDownloader()

        # Extract version from URL (e.g., "55.03.34" from release tag)
        self.version = self._extract_version_from_url(platform_url)

        # Get platform path from cache
        self.platform_path = cache.get_platform_path(platform_url, self.version)

    @staticmethod
    def _extract_version_from_url(url: str) -> str:
        """Extract version string from platform URL.

        Args:
            url: Platform URL (e.g., https://github.com/.../55.03.34/platform.zip)

        Returns:
            Version string (e.g., "55.03.34")
        """
        # URL format: .../releases/download/{version}/platform-espressif32.zip
        parts = url.split("/")
        for i, part in enumerate(parts):
            if part == "download" and i + 1 < len(parts):
                return parts[i + 1]

        # Fallback: use URL hash if version extraction fails
        from .cache import Cache

        return Cache.hash_url(url)[:8]

    def ensure_package(self) -> Path:
        """Ensure platform is downloaded and extracted.

        Returns:
            Path to the extracted platform directory

        Raises:
            PlatformErrorESP32: If download or extraction fails
        """
        return self.ensure_platform()

    def ensure_platform(self) -> Path:
        """Ensure platform is downloaded and extracted.

        Returns:
            Path to the extracted platform directory

        Raises:
            PlatformErrorESP32: If download or extraction fails
        """
        if self.is_installed():
            if self.show_progress:
                print(f"Using cached ESP32 platform {self.version}")
            return self.platform_path

        try:
            if self.show_progress:
                print(f"Downloading ESP32 platform {self.version}...")

            # Download and extract platform package
            self.cache.ensure_directories()

            # Use downloader to handle download and extraction
            archive_name = Path(self.platform_url).name
            archive_path = self.platform_path.parent / archive_name

            # Download if not cached
            if not archive_path.exists():
                archive_path.parent.mkdir(parents=True, exist_ok=True)
                self.downloader.download(
                    self.platform_url, archive_path, show_progress=self.show_progress
                )
            else:
                if self.show_progress:
                    print(f"Using cached archive {archive_name}")

            # Extract to platform directory
            if self.show_progress:
                print(f"Extracting platform to {self.platform_path}...")

            # Create temp extraction directory
            temp_extract = self.platform_path.parent / "temp_extract"
            temp_extract.mkdir(parents=True, exist_ok=True)

            self.downloader.extract_archive(
                archive_path, temp_extract, show_progress=self.show_progress
            )

            # Find the platform directory in the extracted content
            # Usually it's a subdirectory like "platform-espressif32/"
            extracted_dirs = list(temp_extract.glob("platform-*"))
            if not extracted_dirs:
                # Maybe it extracted directly
                extracted_dirs = [temp_extract]

            source_dir = extracted_dirs[0]

            # Move to final location
            if self.platform_path.exists():
                import shutil

                shutil.rmtree(self.platform_path)

            source_dir.rename(self.platform_path)

            # Clean up temp directory
            if temp_extract.exists() and temp_extract != self.platform_path:
                import shutil

                shutil.rmtree(temp_extract, ignore_errors=True)

            if self.show_progress:
                print(f"ESP32 platform installed to {self.platform_path}")

            return self.platform_path

        except (DownloadError, ExtractionError) as e:
            raise PlatformErrorESP32(f"Failed to install ESP32 platform: {e}")
        except KeyboardInterrupt as ke:
            from zapio.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise PlatformErrorESP32(f"Unexpected error installing platform: {e}")

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

    def get_platform_json(self) -> Dict[str, Any]:
        """Load and parse platform.json metadata.

        Returns:
            Dictionary containing platform metadata

        Raises:
            PlatformErrorESP32: If platform.json doesn't exist or is invalid
        """
        platform_json_path = self.platform_path / "platform.json"

        if not platform_json_path.exists():
            raise PlatformErrorESP32(
                f"platform.json not found at {platform_json_path}. "
                + "Ensure platform is downloaded first."
            )

        try:
            with open(platform_json_path, "r") as f:
                return json.load(f)
        except json.JSONDecodeError as e:
            raise PlatformErrorESP32(f"Failed to parse platform.json: {e}")
        except KeyboardInterrupt as ke:
            from zapio.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise PlatformErrorESP32(f"Failed to read platform.json: {e}")

    def get_package_url(self, package_name: str) -> Optional[str]:
        """Get download URL for a specific package.

        Args:
            package_name: Name of the package (e.g., "toolchain-riscv32-esp")

        Returns:
            URL string or None if package not found
        """
        platform_json = self.get_platform_json()
        packages = platform_json.get("packages", {})

        if package_name not in packages:
            return None

        package_info = packages[package_name]
        return package_info.get("version")  # "version" field contains URL

    def get_required_packages(self, board_mcu: str) -> Dict[str, str]:
        """Get required packages for a specific MCU.

        Args:
            board_mcu: MCU type (e.g., "esp32c6", "esp32s3", "esp32")

        Returns:
            Dictionary of package_name -> url for required packages
        """
        packages = {}

        # All ESP32 boards need the Arduino framework
        framework_url = self.get_package_url("framework-arduinoespressif32")
        if framework_url:
            packages["framework-arduinoespressif32"] = framework_url

        libs_url = self.get_package_url("framework-arduinoespressif32-libs")
        if libs_url:
            packages["framework-arduinoespressif32-libs"] = libs_url

        # Determine which toolchain is needed based on MCU architecture
        if board_mcu in ["esp32c3", "esp32c6", "esp32h2", "esp32c2", "esp32c5"]:
            # RISC-V based ESP32s
            toolchain_url = self.get_package_url("toolchain-riscv32-esp")
            if toolchain_url:
                packages["toolchain-riscv32-esp"] = toolchain_url
        else:
            # Xtensa based ESP32s (original ESP32, S2, S3, P4)
            toolchain_url = self.get_package_url("toolchain-xtensa-esp-elf")
            if toolchain_url:
                packages["toolchain-xtensa-esp-elf"] = toolchain_url

        # Add esptool (needed for all ESP32 boards)
        esptool_url = self.get_package_url("tool-esptoolpy")
        if esptool_url:
            packages["tool-esptoolpy"] = esptool_url

        return packages

    def get_boards_dir(self) -> Path:
        """Get path to boards directory.

        Returns:
            Path to boards directory containing JSON board definitions
        """
        return self.platform_path / "boards"

    def get_board_json(self, board_id: str) -> Dict[str, Any]:
        """Load board configuration from JSON.

        Args:
            board_id: Board identifier (e.g., "esp32-c6-devkitm-1")

        Returns:
            Dictionary containing board configuration

        Raises:
            PlatformErrorESP32: If board JSON doesn't exist or is invalid
        """
        board_json_path = self.get_boards_dir() / f"{board_id}.json"

        if not board_json_path.exists():
            raise PlatformErrorESP32(
                f"Board definition not found: {board_id} " + f"at {board_json_path}"
            )

        try:
            with open(board_json_path, "r") as f:
                return json.load(f)
        except json.JSONDecodeError as e:
            raise PlatformErrorESP32(f"Failed to parse board JSON: {e}")
        except KeyboardInterrupt as ke:
            from zapio.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise PlatformErrorESP32(f"Failed to read board JSON: {e}")

    def list_boards(self) -> list[str]:
        """List all available board IDs.

        Returns:
            List of board identifiers
        """
        boards_dir = self.get_boards_dir()
        if not boards_dir.exists():
            return []

        return [
            f.stem
            for f in boards_dir.glob("*.json")
            if f.is_file() and not f.name.endswith(".py")
        ]

    def get_package_info(self) -> Dict[str, Any]:
        """Get information about the installed platform.

        Returns:
            Dictionary with platform information
        """
        return self.get_platform_info()

    def get_platform_info(self) -> Dict[str, Any]:
        """Get information about the installed platform.

        Returns:
            Dictionary with platform information
        """
        info = {
            "version": self.version,
            "path": str(self.platform_path),
            "url": self.platform_url,
            "installed": self.is_installed(),
        }

        if self.is_installed():
            info["boards_dir"] = str(self.get_boards_dir())
            info["available_boards"] = len(self.list_boards())

            # Get package information
            try:
                platform_json = self.get_platform_json()
                info["platform_version"] = platform_json.get("version")
                info["available_packages"] = list(
                    platform_json.get("packages", {}).keys()
                )
            except PlatformErrorESP32:
                pass

        return info
