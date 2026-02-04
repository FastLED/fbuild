"""Teensy Framework Management.

This module handles downloading, extracting, and managing the Teensy Arduino core
framework needed for Teensy 4.x builds.

Framework Download Process:
    1. Download Teensy cores from GitHub (PaulStoffregen/cores)
    2. Extract to cache directory
    3. Provide access to cores, linker scripts, and libraries

Framework Structure (after extraction):
    cores/
    ├── teensy/     # Teensy 2.0 (AVR)
    ├── teensy3/    # Teensy 3.x (Cortex-M4)
    └── teensy4/    # Teensy 4.x (Cortex-M7) <- TARGET
        ├── Arduino.h
        ├── main.cpp
        ├── wiring.c
        ├── core_pins.h
        ├── imxrt.h
        ├── imxrt1062_t41.ld    # Linker script for Teensy 4.1
        └── ...

Key Features:
    - Full Arduino API compatibility
    - USB device type configuration
    - DMA-based peripherals
    - Hardware timers and PWM
"""

from pathlib import Path
from typing import Any, Dict, List, Optional

from .cache import Cache
from .downloader import DownloadError, ExtractionError, PackageDownloader
from .package import IFramework, PackageError


class FrameworkErrorTeensy(PackageError):
    """Raised when Teensy framework operations fail."""

    pass


class FrameworkTeensy(IFramework):
    """Manages Teensy framework download, extraction, and access.

    This class handles the Teensy cores framework which includes:
    - Arduino core for Teensy 4.x (cores/teensy4/)
    - Linker scripts for memory layout
    - Core libraries and headers
    """

    # Teensy cores repository URL
    CORES_REPO_URL = "https://github.com/PaulStoffregen/cores"
    CORES_ARCHIVE_URL = "https://github.com/PaulStoffregen/cores/archive/refs/heads/master.zip"

    def __init__(
        self,
        cache: Cache,
        show_progress: bool = True,
    ):
        """Initialize Teensy framework manager.

        Args:
            cache: Cache manager instance
            show_progress: Whether to show download/extraction progress
        """
        self.cache = cache
        self.show_progress = show_progress
        self.downloader = PackageDownloader()

        # Use master branch as version
        self.version = "master"
        self.framework_url = self.CORES_ARCHIVE_URL

        # Get framework path from cache
        self.framework_path = cache.get_platform_path(self.framework_url, self.version)

    def ensure_framework(self) -> Path:
        """Ensure framework is downloaded and extracted.

        Returns:
            Path to the extracted framework directory

        Raises:
            FrameworkErrorTeensy: If download or extraction fails
        """
        if self.is_installed():
            if self.show_progress:
                print(f"Using cached Teensy cores {self.version}")
            return self.framework_path

        try:
            if self.show_progress:
                print(f"Downloading Teensy cores {self.version}...")

            # Download and extract framework package
            self.cache.ensure_directories()

            # Use downloader to handle download and extraction
            archive_name = "teensy-cores-master.zip"
            archive_path = self.framework_path.parent / archive_name

            # Download if not cached
            if not archive_path.exists():
                archive_path.parent.mkdir(parents=True, exist_ok=True)
                self.downloader.download(self.framework_url, archive_path, show_progress=self.show_progress)
            else:
                if self.show_progress:
                    print("Using cached cores archive")

            # Extract to framework directory
            if self.show_progress:
                print("Extracting Teensy cores...")

            # Create temp extraction directory
            temp_extract = self.framework_path.parent / "temp_extract"
            temp_extract.mkdir(parents=True, exist_ok=True)

            self.downloader.extract_archive(archive_path, temp_extract, show_progress=self.show_progress)

            # Find the cores directory in the extracted content
            # Usually it's a subdirectory like "cores-master/"
            extracted_dirs = list(temp_extract.glob("cores-*"))
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
                print(f"Teensy cores installed to {self.framework_path}")

            return self.framework_path

        except (DownloadError, ExtractionError) as e:
            raise FrameworkErrorTeensy(f"Failed to install Teensy cores: {e}")
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise FrameworkErrorTeensy(f"Unexpected error installing framework: {e}")

    def is_installed(self) -> bool:
        """Check if framework is already installed.

        Returns:
            True if framework directory exists with key files
        """
        if not self.framework_path.exists():
            return False

        # Verify teensy4 core directory exists
        teensy4_path = self.framework_path / "teensy4"
        if not teensy4_path.exists():
            return False

        # Verify essential files exist
        required_files = [
            teensy4_path / "Arduino.h",
            teensy4_path / "main.cpp",
        ]

        return all(f.exists() for f in required_files)

    def get_core_dir(self, core_name: str = "teensy4") -> Path:
        """Get path to specific core directory.

        Args:
            core_name: Core name (default: "teensy4")

        Returns:
            Path to the core directory

        Raises:
            FrameworkErrorTeensy: If core directory doesn't exist
        """
        core_path = self.framework_path / core_name
        if not core_path.exists():
            raise FrameworkErrorTeensy(f"Core '{core_name}' not found at {core_path}")
        return core_path

    def get_core_sources(self, core_name: str = "teensy4") -> List[Path]:
        """Get all source files in a core.

        Args:
            core_name: Core name (default: "teensy4")

        Returns:
            List of .c and .cpp source file paths
        """
        core_dir = self.get_core_dir(core_name)
        sources: List[Path] = []

        # Get all .c and .cpp files in the core directory
        sources.extend(core_dir.glob("*.c"))
        sources.extend(core_dir.glob("*.cpp"))

        # Remove duplicates and sort
        return sorted(set(sources))

    def get_core_includes(self, core_name: str = "teensy4") -> List[Path]:
        """Get include directories for a core.

        Args:
            core_name: Core name (default: "teensy4")

        Returns:
            List of include directory paths
        """
        core_dir = self.get_core_dir(core_name)
        return [core_dir]

    def get_linker_script(self, board: str = "teensy41") -> Optional[Path]:
        """Get linker script for a specific board.

        Args:
            board: Board identifier (default: "teensy41")

        Returns:
            Path to linker script or None if not found
        """
        # Load board configuration to get core and linker script info
        from .. import platform_configs

        config = platform_configs.load_board_config(board)
        if not config:
            return None

        # Get core directory from config (type-safe field access)
        core_name = config.core or "teensy4"
        core_dir = self.get_core_dir(core_name)

        # Get linker script name(s) from config (type-safe field access)
        linker_scripts = config.linker_scripts
        if not linker_scripts:
            # Fallback: use MCU name for linker script (e.g., mkl26z64.ld)
            if config.mcu:
                linker_scripts = [f"{config.mcu}.ld"]

        # Return first linker script that exists
        for script_name in linker_scripts:
            linker_script_path = core_dir / script_name
            if linker_script_path.exists():
                return linker_script_path

        return None

    def list_cores(self) -> List[str]:
        """List all available cores.

        Returns:
            List of core names
        """
        if not self.framework_path.exists():
            return []

        return [d.name for d in self.framework_path.iterdir() if d.is_dir() and d.name.startswith("teensy")]

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
            teensy4_dir = self.framework_path / "teensy4"
            if teensy4_dir.exists():
                info["teensy4_path"] = str(teensy4_dir)
                info["teensy4_sources"] = len(self.get_core_sources("teensy4"))

        return info

    # Implement IFramework interface methods
    def get_cores_dir(self) -> Path:
        """Get path to cores directory.

        Returns:
            Path to cores directory containing Arduino core implementation

        Raises:
            FrameworkErrorTeensy: If cores directory doesn't exist
        """
        if not self.framework_path.exists():
            raise FrameworkErrorTeensy(f"Framework not installed at {self.framework_path}")
        return self.framework_path

    def get_variants_dir(self) -> Path:
        """Get path to variants directory.

        For Teensy, variants are embedded in the core directories.
        Returns the framework path as variants are core-specific.

        Returns:
            Path to framework directory (variants are in core dirs)
        """
        if not self.framework_path.exists():
            raise FrameworkErrorTeensy(f"Framework not installed at {self.framework_path}")
        return self.framework_path

    def get_libraries_dir(self) -> Path:
        """Get path to built-in libraries directory.

        For Teensy, built-in libraries are typically part of Teensyduino.
        Returns the framework path as a base.

        Returns:
            Path to framework directory (libraries location)
        """
        if not self.framework_path.exists():
            raise FrameworkErrorTeensy(f"Framework not installed at {self.framework_path}")
        return self.framework_path

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
