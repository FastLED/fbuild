"""Arduino core platform management.

This module handles downloading and managing Arduino core platforms
(e.g., ArduinoCore-avr) required for building Arduino sketches.
"""

from pathlib import Path
from typing import Optional

from .cache import Cache
from .downloader import PackageDownloader


class ArduinoCoreError(Exception):
    """Raised when Arduino core operations fail."""

    pass


class ArduinoCore:
    """Manages Arduino core platform packages."""

    # Arduino AVR core version
    AVR_VERSION = "1.8.6"

    # Package URL and checksum
    # The Arduino AVR core is hosted on GitHub
    AVR_URL = f"https://github.com/arduino/ArduinoCore-avr/archive/refs/tags/{AVR_VERSION}.tar.gz"
    AVR_CHECKSUM = "49241fd5e504482b94954b5843c7d69ce38ebc1ab47ad3b677e8bb77e0cb8fe6"

    def __init__(self, cache: Cache):
        """Initialize Arduino core manager.

        Args:
            cache: Cache instance for storing cores
        """
        self.cache = cache
        self.downloader = PackageDownloader()
        self._core_path: Optional[Path] = None

    def ensure_avr_core(self, force_download: bool = False) -> Path:
        """Ensure Arduino AVR core is available.

        Args:
            force_download: Force re-download even if cached

        Returns:
            Path to Arduino AVR core directory

        Raises:
            ArduinoCoreError: If core cannot be obtained
        """
        # Check if already loaded
        if self._core_path and not force_download:
            return self._core_path

        # Use URL-based caching
        core_path = self.cache.get_platform_path(self.AVR_URL, self.AVR_VERSION)
        package_name = f"avr-{self.AVR_VERSION}.tar.gz"
        package_path = self.cache.get_package_path(
            self.AVR_URL, self.AVR_VERSION, package_name
        )

        # Check if already extracted and valid
        if not force_download and self.cache.is_platform_cached(
            self.AVR_URL, self.AVR_VERSION
        ):
            if self._verify_core(core_path):
                self._core_path = core_path
                return core_path
            else:
                print("Cached Arduino core failed validation, re-downloading...")

        # Need to download and extract
        self.cache.ensure_directories()

        print(f"Downloading Arduino AVR core ({self.AVR_VERSION})...")

        try:
            # Ensure package directory exists
            package_path.parent.mkdir(parents=True, exist_ok=True)

            # Download if not cached
            if force_download or not package_path.exists():
                self.downloader.download(self.AVR_URL, package_path, self.AVR_CHECKSUM)
            else:
                print(f"Using cached {package_name}")

            # Extract
            print("Extracting Arduino core...")
            core_path.parent.mkdir(parents=True, exist_ok=True)

            # Extract to temporary location first
            import shutil
            import tempfile

            with tempfile.TemporaryDirectory() as temp_dir:
                temp_path = Path(temp_dir)
                self.downloader.extract_archive(
                    package_path, temp_path, show_progress=False
                )

                # The archive extracts to avr/ subdirectory
                extracted_dir = temp_path / "avr"
                if not extracted_dir.exists():
                    # If not in avr/ subdirectory, use first directory found
                    extracted_dirs = [d for d in temp_path.iterdir() if d.is_dir()]
                    if extracted_dirs:
                        extracted_dir = extracted_dirs[0]
                    else:
                        raise ArduinoCoreError(
                            "No directory found in extracted archive"
                        )

                # Move to final location
                if core_path.exists():
                    shutil.rmtree(core_path)
                shutil.move(str(extracted_dir), str(core_path))

            # Verify installation
            if not self._verify_core(core_path):
                raise ArduinoCoreError("Core verification failed after extraction")

            self._core_path = core_path
            print(f"Arduino core ready at {core_path}")
            return core_path

        except Exception as e:
            raise ArduinoCoreError(f"Failed to setup Arduino core: {e}")

    def _verify_core(self, core_path: Path) -> bool:
        """Comprehensively verify Arduino core is complete.

        Checks for:
        - Required directories (cores/arduino, variants)
        - Configuration files (boards.txt, platform.txt)
        - Key core source files
        - Key header files

        Args:
            core_path: Path to core directory

        Returns:
            True if core appears valid
        """
        # Check for essential directories
        required_dirs = [
            "cores/arduino",
            "variants",
            "variants/standard",  # Uno variant
        ]

        for dir_path in required_dirs:
            if not (core_path / dir_path).exists():
                print(f"Missing directory: {dir_path}")
                return False

        # Check for essential configuration files
        required_files = [
            "boards.txt",
            "platform.txt",
        ]

        for file_path in required_files:
            if not (core_path / file_path).exists():
                print(f"Missing file: {file_path}")
                return False

        # Check for key core header files
        required_headers = [
            "cores/arduino/Arduino.h",
            "cores/arduino/HardwareSerial.h",
            "variants/standard/pins_arduino.h",
        ]

        for header in required_headers:
            if not (core_path / header).exists():
                print(f"Missing header: {header}")
                return False

        # Check for key core source files
        required_sources = [
            "cores/arduino/main.cpp",
            "cores/arduino/wiring.c",
            "cores/arduino/wiring_digital.c",
        ]

        for source in required_sources:
            if not (core_path / source).exists():
                print(f"Missing source: {source}")
                return False

        # Verify boards.txt contains uno configuration
        boards_txt = core_path / "boards.txt"
        try:
            content = boards_txt.read_text(encoding="utf-8", errors="ignore")
            if "uno.name" not in content:
                print("boards.txt missing uno configuration")
                return False
        except Exception as e:
            print(f"Failed to read boards.txt: {e}")
            return False

        return True

    def get_boards_txt(self) -> Path:
        """Get path to boards.txt file.

        Returns:
            Path to boards.txt

        Raises:
            ArduinoCoreError: If core not initialized
        """
        if not self._core_path:
            raise ArduinoCoreError(
                "Core not initialized. Call ensure_avr_core() first."
            )

        boards_txt = self._core_path / "boards.txt"
        if not boards_txt.exists():
            raise ArduinoCoreError("boards.txt not found in core")

        return boards_txt

    def get_platform_txt(self) -> Path:
        """Get path to platform.txt file.

        Returns:
            Path to platform.txt

        Raises:
            ArduinoCoreError: If core not initialized
        """
        if not self._core_path:
            raise ArduinoCoreError(
                "Core not initialized. Call ensure_avr_core() first."
            )

        platform_txt = self._core_path / "platform.txt"
        if not platform_txt.exists():
            raise ArduinoCoreError("platform.txt not found in core")

        return platform_txt

    def get_core_dir(self) -> Path:
        """Get path to cores/arduino directory.

        Returns:
            Path to core library sources

        Raises:
            ArduinoCoreError: If core not initialized
        """
        if not self._core_path:
            raise ArduinoCoreError(
                "Core not initialized. Call ensure_avr_core() first."
            )

        core_dir = self._core_path / "cores" / "arduino"
        if not core_dir.exists():
            raise ArduinoCoreError("cores/arduino directory not found")

        return core_dir

    def get_variant_dir(self, variant_name: str = "standard") -> Path:
        """Get path to a board variant directory.

        Args:
            variant_name: Name of the variant (e.g., 'standard' for Uno)

        Returns:
            Path to variant directory

        Raises:
            ArduinoCoreError: If core not initialized or variant not found
        """
        if not self._core_path:
            raise ArduinoCoreError(
                "Core not initialized. Call ensure_avr_core() first."
            )

        variant_dir = self._core_path / "variants" / variant_name
        if not variant_dir.exists():
            raise ArduinoCoreError(f"Variant '{variant_name}' not found")

        return variant_dir

    def get_core_sources(self) -> list[Path]:
        """Get list of all core source files (.c and .cpp).

        Returns:
            List of paths to core source files

        Raises:
            ArduinoCoreError: If core not initialized
        """
        core_dir = self.get_core_dir()

        sources: list[Path] = []
        for pattern in ("*.c", "*.cpp"):
            sources.extend(core_dir.glob(pattern))

        return sorted(sources)

    def get_variant_sources(self, variant_name: str = "standard") -> list[Path]:
        """Get list of variant source files.

        Args:
            variant_name: Name of the variant

        Returns:
            List of paths to variant source files
        """
        variant_dir = self.get_variant_dir(variant_name)

        sources: list[Path] = []
        for pattern in ("*.c", "*.cpp"):
            sources.extend(variant_dir.glob(pattern))

        return sorted(sources)
