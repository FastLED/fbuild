"""RP2040/RP2350 Framework Management.

This module handles downloading, extracting, and managing the Arduino-Pico framework
needed for Raspberry Pi Pico (RP2040) and Pico 2 (RP2350) builds.

Framework Download Process:
    1. Download arduino-pico from GitHub (earlephilhower/arduino-pico)
    2. Extract to cache directory
    3. Provide access to cores, variants, and libraries

Framework Structure (after extraction):
    cores/
    └── rp2040/     # Arduino core for RP2040/RP2350
        ├── Arduino.h
        ├── main.cpp
        ├── wiring.c
        ├── api/
        └── ...
    variants/
    ├── rpipico/    # Raspberry Pi Pico
    ├── rpipicow/   # Raspberry Pi Pico W
    ├── rpipico2/   # Raspberry Pi Pico 2
    └── ...

Key Features:
    - Full Arduino API compatibility
    - Dual-core RP2040 (Cortex-M0+) support
    - Dual-core RP2350 (Cortex-M33) support
    - USB device support
    - WiFi support (Pico W models)
"""

from pathlib import Path
from typing import Any, Dict, List, Optional

from .cache import Cache
from .downloader import DownloadError, ExtractionError, PackageDownloader
from .package import IFramework, PackageError


class FrameworkErrorRP2040(PackageError):
    """Raised when RP2040/RP2350 framework operations fail."""

    pass


class FrameworkRP2040(IFramework):
    """Manages RP2040/RP2350 framework download, extraction, and access.

    This class handles the arduino-pico framework which includes:
    - Arduino core for RP2040/RP2350 (cores/rp2040/)
    - Board variants (variants/)
    - Built-in libraries
    """

    # arduino-pico repository URL
    FRAMEWORK_REPO_URL = "https://github.com/earlephilhower/arduino-pico"
    FRAMEWORK_ARCHIVE_URL = "https://github.com/earlephilhower/arduino-pico/archive/refs/heads/master.zip"

    def __init__(
        self,
        cache: Cache,
        show_progress: bool = True,
    ):
        """Initialize RP2040/RP2350 framework manager.

        Args:
            cache: Cache manager instance
            show_progress: Whether to show download/extraction progress
        """
        self.cache = cache
        self.show_progress = show_progress
        self.downloader = PackageDownloader()

        # Use master branch as version
        self.version = "master"
        self.framework_url = self.FRAMEWORK_ARCHIVE_URL

        # Get framework path from cache
        self.framework_path = cache.get_platform_path(self.framework_url, self.version)

    def ensure_framework(self) -> Path:
        """Ensure framework is downloaded and extracted.

        Returns:
            Path to the extracted framework directory

        Raises:
            FrameworkErrorRP2040: If download or extraction fails
        """
        if self.is_installed():
            if self.show_progress:
                print(f"Using cached arduino-pico {self.version}")
            return self.framework_path

        try:
            if self.show_progress:
                print(f"Downloading arduino-pico {self.version}...")

            # Download and extract framework package
            self.cache.ensure_directories()

            # Use downloader to handle download and extraction
            archive_name = "arduino-pico-master.zip"
            archive_path = self.framework_path.parent / archive_name

            # Download if not cached
            if not archive_path.exists():
                archive_path.parent.mkdir(parents=True, exist_ok=True)
                self.downloader.download(self.framework_url, archive_path, show_progress=self.show_progress)
            else:
                if self.show_progress:
                    print("Using cached arduino-pico archive")

            # Extract to framework directory
            if self.show_progress:
                print("Extracting arduino-pico...")

            # Create temp extraction directory
            temp_extract = self.framework_path.parent / "temp_extract"
            temp_extract.mkdir(parents=True, exist_ok=True)

            self.downloader.extract_archive(archive_path, temp_extract, show_progress=self.show_progress)

            # Find the arduino-pico directory in the extracted content
            # Usually it's a subdirectory like "arduino-pico-master/"
            extracted_dirs = list(temp_extract.glob("arduino-pico-*"))
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
                print(f"arduino-pico installed to {self.framework_path}")

            return self.framework_path

        except (DownloadError, ExtractionError) as e:
            raise FrameworkErrorRP2040(f"Failed to install arduino-pico: {e}")
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise FrameworkErrorRP2040(f"Unexpected error installing framework: {e}")

    def is_installed(self) -> bool:
        """Check if framework is already installed.

        Returns:
            True if framework directory exists with key files
        """
        if not self.framework_path.exists():
            return False

        # Verify rp2040 core directory exists
        rp2040_path = self.framework_path / "cores" / "rp2040"
        if not rp2040_path.exists():
            return False

        # Verify essential files exist
        required_files = [
            rp2040_path / "Arduino.h",
            rp2040_path / "main.cpp",
        ]

        return all(f.exists() for f in required_files)

    def get_core_dir(self, core_name: str = "rp2040") -> Path:
        """Get path to specific core directory.

        Args:
            core_name: Core name (default: "rp2040")

        Returns:
            Path to the core directory

        Raises:
            FrameworkErrorRP2040: If core directory doesn't exist
        """
        core_path = self.framework_path / "cores" / core_name
        if not core_path.exists():
            raise FrameworkErrorRP2040(f"Core '{core_name}' not found at {core_path}")
        return core_path

    def get_core_sources(self, core_name: str = "rp2040") -> List[Path]:
        """Get all source files in a core.

        Args:
            core_name: Core name (default: "rp2040")

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

    def get_core_includes(self, core_name: str = "rp2040") -> List[Path]:
        """Get include directories for a core.

        Args:
            core_name: Core name (default: "rp2040")

        Returns:
            List of include directory paths
        """
        core_dir = self.get_core_dir(core_name)
        includes = [core_dir]

        # Add api directory if it exists
        api_dir = core_dir / "api"
        if api_dir.exists():
            includes.append(api_dir)

        return includes

    def get_variant_dir(self, variant: str = "rpipico") -> Optional[Path]:
        """Get variant directory for a specific board.

        Args:
            variant: Variant identifier (default: "rpipico")

        Returns:
            Path to variant directory or None if not found
        """
        variants_dir = self.framework_path / "variants"
        variant_path = variants_dir / variant

        return variant_path if variant_path.exists() else None

    def list_cores(self) -> List[str]:
        """List all available cores.

        Returns:
            List of core names
        """
        cores_dir = self.framework_path / "cores"
        if not cores_dir.exists():
            return []

        return [d.name for d in cores_dir.iterdir() if d.is_dir()]

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
            rp2040_dir = self.framework_path / "cores" / "rp2040"
            if rp2040_dir.exists():
                info["rp2040_path"] = str(rp2040_dir)
                info["rp2040_sources"] = len(self.get_core_sources("rp2040"))

        return info

    # Implement IFramework interface methods
    def get_cores_dir(self) -> Path:
        """Get path to cores directory.

        Returns:
            Path to cores directory containing Arduino core implementation

        Raises:
            FrameworkErrorRP2040: If cores directory doesn't exist
        """
        cores_dir = self.framework_path / "cores"
        if not cores_dir.exists():
            raise FrameworkErrorRP2040(f"Cores directory not found at {cores_dir}")
        return cores_dir

    def get_variants_dir(self) -> Path:
        """Get path to variants directory.

        Returns:
            Path to variants directory

        Raises:
            FrameworkErrorRP2040: If variants directory doesn't exist
        """
        variants_dir = self.framework_path / "variants"
        if not variants_dir.exists():
            raise FrameworkErrorRP2040(f"Variants directory not found at {variants_dir}")
        return variants_dir

    def get_libraries_dir(self) -> Path:
        """Get path to built-in libraries directory.

        Returns:
            Path to libraries directory

        Raises:
            FrameworkErrorRP2040: If libraries directory doesn't exist
        """
        libraries_dir = self.framework_path / "libraries"
        if not libraries_dir.exists():
            raise FrameworkErrorRP2040(f"Libraries directory not found at {libraries_dir}")
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
