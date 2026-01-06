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

import json
import tarfile
from pathlib import Path
from typing import Any, Dict, List, Optional

from .cache import Cache
from .downloader import DownloadError, ExtractionError, PackageDownloader


class ESP32FrameworkError(Exception):
    """Raised when ESP32 framework operations fail."""

    pass


class ESP32Framework:
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
        show_progress: bool = True,
    ):
        """Initialize ESP32 framework manager.

        Args:
            cache: Cache manager instance
            framework_url: URL to Arduino-ESP32 core package (.tar.xz)
            libs_url: URL to ESP-IDF precompiled libraries (.tar.xz)
            show_progress: Whether to show download/extraction progress
        """
        self.cache = cache
        self.framework_url = framework_url
        self.libs_url = libs_url
        self.show_progress = show_progress
        self.downloader = PackageDownloader()

        # Extract version from URL (e.g., "3.3.4" from release tag)
        self.version = self._extract_version_from_url(framework_url)

        # Get framework paths from cache
        # We'll use a combined hash for both URLs to keep them together
        combined_url = f"{framework_url}|{libs_url}"
        self.framework_path = cache.get_platform_path(combined_url, self.version)

    @staticmethod
    def _extract_version_from_url(url: str) -> str:
        """Extract version string from framework URL.

        Args:
            url: Framework URL (e.g., https://github.com/.../download/3.3.4/esp32-3.3.4.tar.xz)

        Returns:
            Version string (e.g., "3.3.4")
        """
        # URL format: .../releases/download/{version}/esp32-{version}.tar.xz
        parts = url.split("/")
        for i, part in enumerate(parts):
            if part == "download" and i + 1 < len(parts):
                version = parts[i + 1]
                # Clean up version (remove any suffixes)
                return version.split("-")[0] if "-" in version else version

        # Fallback: extract from filename
        filename = url.split("/")[-1]
        if "esp32-" in filename:
            version_part = filename.replace("esp32-", "").replace(".tar.xz", "")
            return version_part.split("-")[0] if "-" in version_part else version_part

        # Last resort: use URL hash
        from .cache import Cache

        return Cache.hash_url(url)[:8]

    def ensure_framework(self) -> Path:
        """Ensure framework is downloaded and extracted.

        Returns:
            Path to the extracted framework directory

        Raises:
            ESP32FrameworkError: If download or extraction fails
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
            self._download_and_extract(
                self.framework_url, self.framework_path, "Arduino-ESP32 core"
            )

            # Download and extract ESP-IDF libraries
            self._download_and_extract(
                self.libs_url,
                self.framework_path / "tools" / "sdk",
                "ESP-IDF libraries",
            )

            if self.show_progress:
                print(f"ESP32 framework installed to {self.framework_path}")

            return self.framework_path

        except (DownloadError, ExtractionError) as e:
            raise ESP32FrameworkError(f"Failed to install ESP32 framework: {e}")
        except Exception as e:
            raise ESP32FrameworkError(f"Unexpected error installing framework: {e}")

    def _download_and_extract(
        self, url: str, target_dir: Path, description: str
    ) -> None:
        """Download and extract a .tar.xz archive.

        Args:
            url: URL to the .tar.xz archive
            target_dir: Directory to extract contents into
            description: Human-readable description for progress messages

        Raises:
            DownloadError: If download fails
            ExtractionError: If extraction fails
        """
        archive_name = Path(url).name
        archive_path = self.framework_path.parent / archive_name

        # Download if not cached
        if not archive_path.exists():
            if self.show_progress:
                print(f"Downloading {description}...")
            archive_path.parent.mkdir(parents=True, exist_ok=True)
            self.downloader.download(
                url, archive_path, show_progress=self.show_progress
            )
        else:
            if self.show_progress:
                print(f"Using cached {description} archive")

        # Extract to target directory
        if self.show_progress:
            print(f"Extracting {description}...")

        # Create temp extraction directory
        temp_extract = target_dir.parent / f"temp_extract_{archive_name}"
        temp_extract.mkdir(parents=True, exist_ok=True)

        try:
            # Extract .tar.xz archive
            with tarfile.open(archive_path, "r:xz") as tar:
                tar.extractall(temp_extract)

            # Find the extracted directory
            # Usually it's a subdirectory like "esp32/" or directly extracted
            extracted_items = list(temp_extract.iterdir())

            if len(extracted_items) == 1 and extracted_items[0].is_dir():
                # Single directory extracted - use its contents
                source_dir = extracted_items[0]
            else:
                # Multiple items or files - use temp_extract as source
                source_dir = temp_extract

            # Move contents to target directory
            target_dir.mkdir(parents=True, exist_ok=True)
            import shutil

            for item in source_dir.iterdir():
                dest = target_dir / item.name
                if item.is_dir():
                    if dest.exists():
                        shutil.rmtree(dest)
                    shutil.copytree(item, dest)
                else:
                    if dest.exists():
                        dest.unlink()
                    shutil.copy2(item, dest)

        finally:
            # Clean up temp directory
            if temp_extract.exists():
                import shutil

                shutil.rmtree(temp_extract, ignore_errors=True)

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
            ESP32FrameworkError: If core directory doesn't exist
        """
        core_path = self.get_cores_dir() / core_name
        if not core_path.exists():
            raise ESP32FrameworkError(f"Core '{core_name}' not found at {core_path}")
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
            ESP32FrameworkError: If variant directory doesn't exist
        """
        variant_path = self.get_variants_dir() / variant_name
        if not variant_path.exists():
            raise ESP32FrameworkError(
                f"Variant '{variant_name}' not found at {variant_path}"
            )
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

        return [
            d.name
            for d in libs_dir.iterdir()
            if d.is_dir() and not d.name.startswith(".")
        ]

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
        # Read the SDK's includes file
        includes_file = self.get_sdk_flags_dir(mcu) / "includes"
        if not includes_file.exists():
            # Fallback to recursive discovery if includes file doesn't exist
            return self._get_sdk_includes_recursive(mcu)

        try:
            # Read includes file (single line with space-separated entries)
            includes_content = includes_file.read_text().strip()

            # Parse the includes: "-iwithprefixbefore path1 -iwithprefixbefore path2 ..."
            # The -iwithprefixbefore flag means to prepend the SDK include directory
            sdk_include_base = self.get_sdk_dir() / mcu / "include"

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

        except Exception as e:
            # Fallback to recursive discovery on error
            if self.show_progress:
                print(f"Warning: Failed to parse includes file: {e}")
                print("Falling back to recursive include discovery")
            return self._get_sdk_includes_recursive(mcu)

    def _get_sdk_includes_recursive(self, mcu: str) -> List[Path]:
        """Fallback method: recursively discover include directories.

        This was the original implementation, now kept as a fallback.
        It discovers 557 paths for esp32c6 but causes C++ stdlib conflicts.

        Args:
            mcu: MCU type (e.g., "esp32c6", "esp32s3")

        Returns:
            List of include directory paths
        """
        sdk_mcu_dir = self.get_sdk_dir() / mcu / "include"
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
        libs = []

        # Get main SDK libraries
        sdk_lib_dir = self.get_sdk_dir() / mcu / "lib"
        if sdk_lib_dir.exists():
            libs.extend(sdk_lib_dir.glob("*.a"))

        # Get flash mode-specific libraries (qio_qspi or dio_qspi)
        # For ESP32-C6: Only libspi_flash.a
        # For ESP32-S3: Multiple libraries including libfreertos.a, libesp_system.a, etc.
        flash_lib_dir = self.get_sdk_dir() / mcu / f"{flash_mode}_qspi"
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
        return self.get_sdk_dir() / mcu / "flags"

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
            ESP32FrameworkError: If package.json doesn't exist or is invalid
        """
        package_json_path = self.framework_path / "package.json"

        if not package_json_path.exists():
            raise ESP32FrameworkError(
                f"package.json not found at {package_json_path}. "
                "Ensure framework is downloaded first."
            )

        try:
            with open(package_json_path, "r", encoding="utf-8") as f:
                return json.load(f)
        except json.JSONDecodeError as e:
            raise ESP32FrameworkError(f"Failed to parse package.json: {e}")
        except Exception as e:
            raise ESP32FrameworkError(f"Failed to read package.json: {e}")

    def list_variants(self) -> List[str]:
        """List all available board variants.

        Returns:
            List of variant names
        """
        variants_dir = self.get_variants_dir()
        if not variants_dir.exists():
            return []

        return [
            d.name
            for d in variants_dir.iterdir()
            if d.is_dir() and not d.name.startswith(".")
        ]

    def list_cores(self) -> List[str]:
        """List all available cores.

        Returns:
            List of core names
        """
        cores_dir = self.get_cores_dir()
        if not cores_dir.exists():
            return []

        return [
            d.name
            for d in cores_dir.iterdir()
            if d.is_dir() and not d.name.startswith(".")
        ]

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
            except ESP32FrameworkError:
                pass

        return info
