"""STM32 Framework Management.

This module handles downloading, extracting, and managing the STM32duino Arduino framework
needed for STM32 Arduino builds.

Framework Download Process:
    1. Download Arduino_Core_STM32 from GitHub (stm32duino/Arduino_Core_STM32)
    2. Extract to cache directory
    3. Provide access to cores, variants, and libraries

Framework Structure (after extraction):
    cores/
    └── arduino/     # Arduino core for STM32
        ├── Arduino.h
        ├── main.cpp
        ├── wiring.c
        └── ...
    variants/
    ├── STM32F1xx/   # STM32F1 variants
    ├── STM32F4xx/   # STM32F4 variants
    └── ...

Supported MCU Families:
    - STM32F0, STM32F1, STM32F2, STM32F3, STM32F4, STM32F7
    - STM32G0, STM32G4
    - STM32H7
    - STM32L0, STM32L1, STM32L4, STM32L5
    - STM32U5
    - STM32WB, STM32WL
"""

from pathlib import Path
from typing import Any, Dict, List, Optional

from .cache import Cache
from .downloader import DownloadError, ExtractionError, PackageDownloader
from .package import IFramework, PackageError


class FrameworkErrorSTM32(PackageError):
    """Raised when STM32 framework operations fail."""

    pass


class FrameworkSTM32(IFramework):
    """Manages STM32 framework download, extraction, and access.

    This class handles the stm32duino Arduino_Core_STM32 framework which includes:
    - Arduino core for STM32 (cores/arduino/)
    - Board variants (variants/)
    - Built-in libraries
    - CMSIS and HAL drivers
    """

    # STM32duino repository URL
    FRAMEWORK_REPO_URL = "https://github.com/stm32duino/Arduino_Core_STM32"
    # Using tagged release for stability
    FRAMEWORK_VERSION = "2.12.0"
    FRAMEWORK_ARCHIVE_URL = f"https://github.com/stm32duino/Arduino_Core_STM32/archive/refs/tags/{FRAMEWORK_VERSION}.zip"

    def __init__(
        self,
        cache: Cache,
        show_progress: bool = True,
    ):
        """Initialize STM32 framework manager.

        Args:
            cache: Cache manager instance
            show_progress: Whether to show download/extraction progress
        """
        self.cache = cache
        self.show_progress = show_progress
        self.downloader = PackageDownloader()

        # Use tagged version
        self.version = self.FRAMEWORK_VERSION
        self.framework_url = self.FRAMEWORK_ARCHIVE_URL

        # Get framework path from cache
        self.framework_path = cache.get_platform_path(self.framework_url, self.version)

    def ensure_framework(self) -> Path:
        """Ensure framework is downloaded and extracted.

        Returns:
            Path to the extracted framework directory

        Raises:
            FrameworkErrorSTM32: If download or extraction fails
        """
        if self.is_installed():
            if self.show_progress:
                print(f"Using cached Arduino_Core_STM32 {self.version}")
            return self.framework_path

        try:
            if self.show_progress:
                print(f"Downloading Arduino_Core_STM32 {self.version}...")

            # Download and extract framework package
            self.cache.ensure_directories()

            # Use downloader to handle download and extraction
            archive_name = f"Arduino_Core_STM32-{self.version}.zip"
            archive_path = self.framework_path.parent / archive_name

            # Download if not cached
            if not archive_path.exists():
                archive_path.parent.mkdir(parents=True, exist_ok=True)
                self.downloader.download(self.framework_url, archive_path, show_progress=self.show_progress)
            else:
                if self.show_progress:
                    print("Using cached Arduino_Core_STM32 archive")

            # Extract to framework directory
            if self.show_progress:
                print("Extracting Arduino_Core_STM32...")

            # Create temp extraction directory
            temp_extract = self.framework_path.parent / "temp_extract"
            temp_extract.mkdir(parents=True, exist_ok=True)

            self.downloader.extract_archive(archive_path, temp_extract, show_progress=self.show_progress)

            # Find the Arduino_Core_STM32 directory in the extracted content
            # Usually it's a subdirectory like "Arduino_Core_STM32-2.12.0/"
            extracted_dirs = list(temp_extract.glob("Arduino_Core_STM32-*"))
            if not extracted_dirs:
                # Maybe it extracted directly
                extracted_dirs = [temp_extract]

            source_dir = extracted_dirs[0]

            # Move to final location
            if self.framework_path.exists():
                import shutil

                shutil.rmtree(self.framework_path)

            source_dir.rename(self.framework_path)

            # Clean up temp directory
            if temp_extract.exists() and temp_extract != self.framework_path:
                import shutil

                shutil.rmtree(temp_extract, ignore_errors=True)

            if self.show_progress:
                print(f"Arduino_Core_STM32 installed to {self.framework_path}")

            return self.framework_path

        except (DownloadError, ExtractionError) as e:
            raise FrameworkErrorSTM32(f"Failed to install Arduino_Core_STM32: {e}")
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise FrameworkErrorSTM32(f"Unexpected error installing framework: {e}")

    def is_installed(self) -> bool:
        """Check if framework is already installed.

        Returns:
            True if framework directory exists with key files
        """
        if not self.framework_path.exists():
            return False

        # Verify arduino core directory exists
        arduino_path = self.framework_path / "cores" / "arduino"
        if not arduino_path.exists():
            return False

        # Verify essential files exist
        required_files = [
            arduino_path / "Arduino.h",
            arduino_path / "main.cpp",
        ]

        return all(f.exists() for f in required_files)

    def get_core_dir(self, core_name: str = "arduino") -> Path:
        """Get path to specific core directory.

        Args:
            core_name: Core name (default: "arduino")

        Returns:
            Path to the core directory

        Raises:
            FrameworkErrorSTM32: If core directory doesn't exist
        """
        core_path = self.framework_path / "cores" / core_name
        if not core_path.exists():
            raise FrameworkErrorSTM32(f"Core '{core_name}' not found at {core_path}")
        return core_path

    def get_core_sources(self, core_name: str = "arduino") -> List[Path]:
        """Get all source files in a core.

        Args:
            core_name: Core name (default: "arduino")

        Returns:
            List of .c and .cpp source file paths
        """
        core_dir = self.get_core_dir(core_name)
        sources: List[Path] = []

        # Get all .c and .cpp files in the core directory (recursively)
        sources.extend(core_dir.rglob("*.c"))
        sources.extend(core_dir.rglob("*.cpp"))

        # Remove duplicates and sort
        return sorted(set(sources))

    def get_core_includes(self, core_name: str = "arduino") -> List[Path]:
        """Get include directories for a core.

        Args:
            core_name: Core name (default: "arduino")

        Returns:
            List of include directory paths
        """
        core_dir = self.get_core_dir(core_name)
        includes = [core_dir]

        # Add api directory if it exists
        api_dir = core_dir / "api"
        if api_dir.exists():
            includes.append(api_dir)

        # Add stm32 directory if it exists
        stm32_dir = core_dir / "stm32"
        if stm32_dir.exists():
            includes.append(stm32_dir)

        # CRITICAL: Add SrcWrapper includes (clock.h, interrupt.h, PinNames.h, etc.)
        srcwrapper_inc = self.framework_path / "libraries" / "SrcWrapper" / "inc"
        if srcwrapper_inc.exists():
            includes.append(srcwrapper_inc)

        return includes

    def get_stm32_system_includes(self, mcu_family: str) -> List[Path]:
        """Get STM32 system include directories for CMSIS and HAL.

        Args:
            mcu_family: MCU family name (e.g., "STM32F4xx", "STM32F1xx")

        Returns:
            List of system include directory paths
        """
        includes = []

        system_dir = self.framework_path / "system"
        if not system_dir.exists():
            return includes

        # CMSIS Core includes
        cmsis_core = system_dir / "Drivers" / "CMSIS" / "Core" / "Include"
        if cmsis_core.exists():
            includes.append(cmsis_core)

        # CMSIS Device includes for specific MCU family
        cmsis_device = system_dir / "Drivers" / "CMSIS" / "Device" / "ST" / mcu_family / "Include"
        if cmsis_device.exists():
            includes.append(cmsis_device)

        # HAL Driver includes for specific MCU family
        hal_driver = system_dir / "Drivers" / f"{mcu_family}_HAL_Driver" / "Inc"
        if hal_driver.exists():
            includes.append(hal_driver)

        # Legacy HAL includes
        hal_legacy = system_dir / "Drivers" / f"{mcu_family}_HAL_Driver" / "Inc" / "Legacy"
        if hal_legacy.exists():
            includes.append(hal_legacy)

        # LL (Low-Layer) driver includes (same as HAL path)
        # Already included above

        # System directory itself
        includes.append(system_dir)

        return includes

    def get_variant_dir(self, variant: str) -> Optional[Path]:
        """Get variant directory for a specific board.

        Args:
            variant: Variant identifier (e.g., "STM32F4xx/F446R(C-E)T")

        Returns:
            Path to variant directory or None if not found
        """
        variants_dir = self.framework_path / "variants"
        variant_path = variants_dir / variant

        return variant_path if variant_path.exists() else None

    def get_system_dir(self) -> Path:
        """Get path to system directory containing CMSIS and HAL.

        Returns:
            Path to system directory

        Raises:
            FrameworkErrorSTM32: If system directory doesn't exist
        """
        system_dir = self.framework_path / "system"
        if not system_dir.exists():
            raise FrameworkErrorSTM32(f"System directory not found at {system_dir}")
        return system_dir

    def get_cmsis_dir(self) -> Optional[Path]:
        """Get path to CMSIS directory.

        Returns:
            Path to CMSIS directory or None if not found
        """
        cmsis_dir = self.framework_path / "system" / "Drivers" / "CMSIS"
        return cmsis_dir if cmsis_dir.exists() else None

    def get_hal_dir(self, mcu_family: str) -> Optional[Path]:
        """Get path to HAL drivers for a specific MCU family.

        Args:
            mcu_family: MCU family name (e.g., "STM32F4xx")

        Returns:
            Path to HAL directory or None if not found
        """
        hal_dir = self.framework_path / "system" / "Drivers" / f"{mcu_family}_HAL_Driver"
        return hal_dir if hal_dir.exists() else None

    def list_cores(self) -> List[str]:
        """List all available cores.

        Returns:
            List of core names
        """
        cores_dir = self.framework_path / "cores"
        if not cores_dir.exists():
            return []

        return [d.name for d in cores_dir.iterdir() if d.is_dir()]

    def list_variants(self) -> List[str]:
        """List all available variants.

        Returns:
            List of variant identifiers
        """
        variants_dir = self.framework_path / "variants"
        if not variants_dir.exists():
            return []

        variants = []
        # Variants are organized by MCU family
        for family_dir in variants_dir.iterdir():
            if family_dir.is_dir():
                for variant_dir in family_dir.iterdir():
                    if variant_dir.is_dir():
                        variants.append(f"{family_dir.name}/{variant_dir.name}")

        return sorted(variants)

    def get_framework_info(self) -> Dict[str, Any]:
        """Get information about the installed framework.

        Returns:
            Dictionary with framework information
        """
        info = {
            "version": self.version,
            "path": str(self.framework_path),
            "url": self.framework_url,
            "installed": self.is_installed(),
        }

        if self.is_installed():
            info["available_cores"] = self.list_cores()
            arduino_dir = self.framework_path / "cores" / "arduino"
            if arduino_dir.exists():
                info["arduino_path"] = str(arduino_dir)
                info["arduino_sources"] = len(self.get_core_sources("arduino"))

        return info

    # Implement IFramework interface methods
    def get_cores_dir(self) -> Path:
        """Get path to cores directory.

        Returns:
            Path to cores directory containing Arduino core implementation

        Raises:
            FrameworkErrorSTM32: If cores directory doesn't exist
        """
        cores_dir = self.framework_path / "cores"
        if not cores_dir.exists():
            raise FrameworkErrorSTM32(f"Cores directory not found at {cores_dir}")
        return cores_dir

    def get_variants_dir(self) -> Path:
        """Get path to variants directory.

        Returns:
            Path to variants directory

        Raises:
            FrameworkErrorSTM32: If variants directory doesn't exist
        """
        variants_dir = self.framework_path / "variants"
        if not variants_dir.exists():
            raise FrameworkErrorSTM32(f"Variants directory not found at {variants_dir}")
        return variants_dir

    def get_libraries_dir(self) -> Path:
        """Get path to built-in libraries directory.

        Returns:
            Path to libraries directory

        Raises:
            FrameworkErrorSTM32: If libraries directory doesn't exist
        """
        libraries_dir = self.framework_path / "libraries"
        if not libraries_dir.exists():
            raise FrameworkErrorSTM32(f"Libraries directory not found at {libraries_dir}")
        return libraries_dir

    # Implement IPackage interface
    def ensure_package(self) -> Path:
        """Ensure package is downloaded and extracted.

        Returns:
            Path to the extracted package directory

        Raises:
            PackageError: If download or extraction fails
        """
        return self.ensure_framework()

    def get_package_info(self) -> Dict[str, Any]:
        """Get information about the package.

        Returns:
            Dictionary with package metadata (version, path, etc.)
        """
        return self.get_framework_info()
