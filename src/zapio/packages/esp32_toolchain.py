"""ESP32 Toolchain Management.

This module handles downloading, extracting, and managing ESP32 toolchains
(RISC-V and Xtensa GCC compilers) needed for ESP32 builds.

Toolchain Download Process:
    1. Download metadata package (contains tools.json with platform-specific URLs)
    2. Parse tools.json to get correct URL for current platform (win64, linux-amd64, etc.)
    3. Download platform-specific toolchain archive
    4. Extract to cache directory

Toolchain Structure (after extraction):
    toolchain-riscv32-esp/          # RISC-V toolchain for C3, C6, H2
    ├── riscv32-esp-elf/
    │   ├── bin/
    │   │   ├── riscv32-esp-elf-gcc.exe
    │   │   ├── riscv32-esp-elf-g++.exe
    │   │   ├── riscv32-esp-elf-ar.exe
    │   │   ├── riscv32-esp-elf-objcopy.exe
    │   │   └── ...
    │   ├── lib/
    │   └── include/

    toolchain-xtensa-esp-elf/       # Xtensa toolchain for ESP32, S2, S3
    ├── xtensa-esp32-elf/
    │   ├── bin/
    │   │   ├── xtensa-esp32-elf-gcc.exe
    │   │   ├── xtensa-esp32-elf-g++.exe
    │   │   ├── xtensa-esp32-elf-ar.exe
    │   │   ├── xtensa-esp32-elf-objcopy.exe
    │   │   └── ...
    │   ├── lib/
    │   └── include/

Supported Architectures:
    - RISC-V: ESP32-C3, ESP32-C6, ESP32-H2, ESP32-C2, ESP32-C5
    - Xtensa: ESP32, ESP32-S2, ESP32-S3, ESP32-P4
"""

import json
import platform
import sys
from pathlib import Path
from typing import Any, Dict, Literal, Optional, cast

from .cache import Cache
from .downloader import DownloadError, ExtractionError, PackageDownloader


class ESP32ToolchainError(Exception):
    """Raised when ESP32 toolchain operations fail."""

    pass


ToolchainType = Literal["riscv32-esp", "xtensa-esp-elf"]


class ESP32Toolchain:
    """Manages ESP32 toolchain download, extraction, and access.

    This class handles downloading and managing GCC toolchains for ESP32 family:
    - RISC-V GCC for ESP32-C3, C6, H2, C2, C5 chips
    - Xtensa GCC for ESP32, S2, S3, P4 chips
    """

    # Toolchain name mappings
    TOOLCHAIN_NAMES = {
        "riscv32-esp": "riscv32-esp-elf",
        "xtensa-esp-elf": "xtensa-esp32-elf",  # Note: xtensa uses esp32 in binary names
    }

    # MCU to toolchain type mapping
    MCU_TOOLCHAIN_MAP = {
        "esp32": "xtensa-esp-elf",
        "esp32s2": "xtensa-esp-elf",
        "esp32s3": "xtensa-esp-elf",
        "esp32p4": "xtensa-esp-elf",
        "esp32c2": "riscv32-esp",
        "esp32c3": "riscv32-esp",
        "esp32c5": "riscv32-esp",
        "esp32c6": "riscv32-esp",
        "esp32h2": "riscv32-esp",
    }

    def __init__(
        self,
        cache: Cache,
        toolchain_url: str,
        toolchain_type: ToolchainType,
        show_progress: bool = True,
    ):
        """Initialize ESP32 toolchain manager.

        Args:
            cache: Cache manager instance
            toolchain_url: URL to toolchain package (e.g., GitHub release ZIP)
            toolchain_type: Type of toolchain ("riscv32-esp" or "xtensa-esp-elf")
            show_progress: Whether to show download/extraction progress
        """
        self.cache = cache
        self.toolchain_url = toolchain_url
        self.toolchain_type = toolchain_type
        self.show_progress = show_progress
        self.downloader = PackageDownloader()

        # Extract version from URL
        self.version = self._extract_version_from_url(toolchain_url)

        # Get toolchain path from cache
        self.toolchain_path = cache.get_toolchain_path(toolchain_url, self.version)

        # Get binary prefix for this toolchain type
        self.binary_prefix = self.TOOLCHAIN_NAMES.get(toolchain_type, toolchain_type)

    @staticmethod
    def _extract_version_from_url(url: str) -> str:
        """Extract version string from toolchain URL.

        Args:
            url: Toolchain URL (e.g., https://github.com/.../riscv32-esp-elf-14.2.0_20250730.zip)

        Returns:
            Version string (e.g., "14.2.0_20250730")
        """
        # URL format: .../registry/releases/download/{version}/{filename}
        # or: .../riscv32-esp-elf-{version}.zip
        filename = url.split("/")[-1]

        # Try to extract version from filename
        # Format: toolchain-name-VERSION.zip
        for prefix in ["riscv32-esp-elf-", "xtensa-esp-elf-"]:
            if prefix in filename:
                version_part = filename.replace(prefix, "").replace(".zip", "")
                return version_part

        # Fallback: use URL hash if version extraction fails
        from .cache import Cache

        return Cache.hash_url(url)[:8]

    @staticmethod
    def get_toolchain_type_for_mcu(mcu: str) -> ToolchainType:
        """Get the toolchain type needed for a specific MCU.

        Args:
            mcu: MCU type (e.g., "esp32c6", "esp32s3", "esp32")

        Returns:
            Toolchain type string ("riscv32-esp" or "xtensa-esp-elf")

        Raises:
            ESP32ToolchainError: If MCU type is unknown
        """
        mcu_lower = mcu.lower()
        if mcu_lower in ESP32Toolchain.MCU_TOOLCHAIN_MAP:
            return cast(ToolchainType, ESP32Toolchain.MCU_TOOLCHAIN_MAP[mcu_lower])

        raise ESP32ToolchainError(f"Unknown MCU type: {mcu}")

    @staticmethod
    def detect_platform() -> str:
        """Detect the current platform for toolchain selection.

        Returns:
            Platform identifier (win32, win64, linux-amd64, linux-arm64, macos, macos-arm64)
        """
        system = platform.system().lower()
        machine = platform.machine().lower()

        if system == "windows":
            # Check if 64-bit
            return "win64" if sys.maxsize > 2**32 else "win32"
        elif system == "linux":
            if "aarch64" in machine or "arm64" in machine:
                return "linux-arm64"
            elif "arm" in machine:
                # Check for hard float vs soft float
                return "linux-armhf"  # Default to hard float
            elif "i686" in machine or "i386" in machine:
                return "linux-i686"
            else:
                return "linux-amd64"
        elif system == "darwin":
            if "arm64" in machine or "aarch64" in machine:
                return "macos-arm64"
            else:
                return "macos"
        else:
            raise ESP32ToolchainError(f"Unsupported platform: {system} {machine}")

    def _get_platform_url_from_metadata(self) -> str:
        """Download metadata package and extract platform-specific toolchain URL.

        Returns:
            URL to platform-specific toolchain archive

        Raises:
            ESP32ToolchainError: If metadata cannot be parsed or platform not found
        """
        # Download metadata package
        metadata_path = self.toolchain_path
        if not metadata_path.exists():
            if self.show_progress:
                print("Downloading toolchain metadata...")

            archive_name = Path(self.toolchain_url).name
            archive_path = metadata_path.parent / archive_name

            if not archive_path.exists():
                archive_path.parent.mkdir(parents=True, exist_ok=True)
                self.downloader.download(
                    self.toolchain_url, archive_path, show_progress=self.show_progress
                )

            # Extract metadata
            temp_extract = metadata_path.parent / "temp_metadata"
            temp_extract.mkdir(parents=True, exist_ok=True)

            self.downloader.extract_archive(
                archive_path, temp_extract, show_progress=False
            )

            # Move to final location
            if metadata_path.exists():
                import shutil

                shutil.rmtree(metadata_path)

            temp_extract.rename(metadata_path)

        # Parse tools.json
        tools_json_path = metadata_path / "tools.json"
        if not tools_json_path.exists():
            raise ESP32ToolchainError("tools.json not found in metadata package")

        with open(tools_json_path, "r") as f:
            tools_data = json.load(f)

        # Find the toolchain tool
        tools = tools_data.get("tools", [])
        for tool in tools:
            if tool.get("name") == f"toolchain-{self.toolchain_type}":
                # Get versions
                versions = tool.get("versions", [])
                if not versions:
                    raise ESP32ToolchainError(
                        f"No versions found for {self.toolchain_type}"
                    )

                # Use the first version (usually the recommended one)
                version_info = versions[0]

                # Detect current platform
                current_platform = self.detect_platform()

                # Get URL for current platform
                if current_platform not in version_info:
                    raise ESP32ToolchainError(
                        f"Platform {current_platform} not supported for {self.toolchain_type}. "
                        f"Available platforms: {list(version_info.keys())}"
                    )

                platform_info = version_info[current_platform]
                return platform_info["url"]

        raise ESP32ToolchainError(
            f"Toolchain {self.toolchain_type} not found in tools.json"
        )

    def ensure_toolchain(self) -> Path:
        """Ensure toolchain is downloaded and extracted.

        Returns:
            Path to the extracted toolchain directory

        Raises:
            ESP32ToolchainError: If download or extraction fails
        """
        if self.is_installed():
            if self.show_progress:
                print(f"Using cached {self.toolchain_type} toolchain {self.version}")
            return self.toolchain_path

        try:
            # Step 1: Get platform-specific URL from metadata
            platform_url = self._get_platform_url_from_metadata()

            if self.show_progress:
                print(
                    f"Downloading {self.toolchain_type} toolchain for {self.detect_platform()}..."
                )

            # Download and extract toolchain package
            self.cache.ensure_directories()

            # Use downloader to handle download and extraction
            archive_name = Path(platform_url).name
            # Use a different path for the actual toolchain (not metadata)
            toolchain_cache_dir = self.toolchain_path.parent / "bin"
            toolchain_cache_dir.mkdir(parents=True, exist_ok=True)
            archive_path = toolchain_cache_dir / archive_name

            # Download if not cached
            if not archive_path.exists():
                self.downloader.download(
                    platform_url, archive_path, show_progress=self.show_progress
                )
            else:
                if self.show_progress:
                    print("Using cached toolchain archive")

            # Extract to toolchain directory
            if self.show_progress:
                print("Extracting toolchain...")

            # Create temp extraction directory
            temp_extract = toolchain_cache_dir / "temp_extract"
            temp_extract.mkdir(parents=True, exist_ok=True)

            self.downloader.extract_archive(
                archive_path, temp_extract, show_progress=self.show_progress
            )

            # Find the toolchain directory in the extracted content
            # Usually it's a subdirectory like "riscv32-esp-elf/" or "xtensa-esp32-elf/"
            extracted_dirs = list(temp_extract.glob("*esp*"))
            if not extracted_dirs:
                # Maybe it extracted directly
                extracted_dirs = [temp_extract]

            source_dir = extracted_dirs[0]

            # Move to final location (toolchain_path/bin)
            final_bin_path = toolchain_cache_dir
            if final_bin_path.exists() and final_bin_path != temp_extract:
                # Remove old installation
                import shutil

                for item in final_bin_path.iterdir():
                    if item.name != "temp_extract" and not item.name.endswith(
                        (".zip", ".tar", ".xz", ".gz")
                    ):
                        if item.is_dir():
                            shutil.rmtree(item)
                        else:
                            item.unlink()

            # Copy contents from source_dir to final_bin_path
            import shutil

            for item in source_dir.iterdir():
                dest = final_bin_path / item.name
                if item.is_dir():
                    if dest.exists():
                        shutil.rmtree(dest)
                    shutil.copytree(item, dest)
                else:
                    if dest.exists():
                        dest.unlink()
                    shutil.copy2(item, dest)

            # Clean up temp directory
            if temp_extract.exists():
                import shutil

                shutil.rmtree(temp_extract, ignore_errors=True)

            if self.show_progress:
                print(f"{self.toolchain_type} toolchain installed successfully")

            return self.toolchain_path

        except (DownloadError, ExtractionError) as e:
            raise ESP32ToolchainError(
                f"Failed to install {self.toolchain_type} toolchain: {e}"
            )
        except Exception as e:
            raise ESP32ToolchainError(f"Unexpected error installing toolchain: {e}")

    def is_installed(self) -> bool:
        """Check if toolchain is already installed.

        Returns:
            True if toolchain directory exists with key binaries
        """
        if not self.toolchain_path.exists():
            return False

        # Verify essential toolchain binaries exist
        gcc_path = self.get_gcc_path()
        return gcc_path is not None and gcc_path.exists()

    def get_bin_dir(self) -> Optional[Path]:
        """Get path to toolchain bin directory.

        Returns:
            Path to bin directory containing compiler binaries, or None if not found
        """
        # The toolchain structure is: toolchain_path/bin/bin/
        # (after extraction, the toolchain extracts to a subdirectory,
        # and we copy it to toolchain_path/bin/)
        bin_parent = self.toolchain_path.parent / "bin"

        if not bin_parent.exists():
            return None

        # First check for bin/bin/ (most common after extraction)
        bin_dir = bin_parent / "bin"
        if bin_dir.exists() and bin_dir.is_dir():
            # Verify it has binaries
            binaries = list(bin_dir.glob("*.exe")) or list(bin_dir.glob("*-gcc"))
            if binaries:
                return bin_dir

        # Look for nested toolchain directory (e.g., bin/riscv32-esp-elf/bin/)
        for item in bin_parent.iterdir():
            if item.is_dir() and "esp" in item.name.lower():
                nested_bin = item / "bin"
                if nested_bin.exists():
                    return nested_bin

        return None

    def _find_binary(self, binary_name: str) -> Optional[Path]:
        """Find a binary in the toolchain bin directory.

        Args:
            binary_name: Name of the binary (e.g., "gcc", "g++")

        Returns:
            Path to binary or None if not found
        """
        bin_dir = self.get_bin_dir()
        if bin_dir is None or not bin_dir.exists():
            return None

        # Try with .exe extension (Windows)
        binary_with_prefix = f"{self.binary_prefix}-{binary_name}"

        # Check both with and without .exe
        for ext in [".exe", ""]:
            binary_path = bin_dir / f"{binary_with_prefix}{ext}"
            if binary_path.exists():
                return binary_path

        return None

    def get_gcc_path(self) -> Optional[Path]:
        """Get path to GCC compiler.

        Returns:
            Path to gcc binary or None if not found
        """
        return self._find_binary("gcc")

    def get_gxx_path(self) -> Optional[Path]:
        """Get path to G++ compiler.

        Returns:
            Path to g++ binary or None if not found
        """
        return self._find_binary("g++")

    def get_ar_path(self) -> Optional[Path]:
        """Get path to archiver (ar).

        Returns:
            Path to ar binary or None if not found
        """
        return self._find_binary("ar")

    def get_objcopy_path(self) -> Optional[Path]:
        """Get path to objcopy utility.

        Returns:
            Path to objcopy binary or None if not found
        """
        return self._find_binary("objcopy")

    def get_size_path(self) -> Optional[Path]:
        """Get path to size utility.

        Returns:
            Path to size binary or None if not found
        """
        return self._find_binary("size")

    def get_objdump_path(self) -> Optional[Path]:
        """Get path to objdump utility.

        Returns:
            Path to objdump binary or None if not found
        """
        return self._find_binary("objdump")

    def get_all_tool_paths(self) -> Dict[str, Optional[Path]]:
        """Get paths to all common toolchain binaries.

        Returns:
            Dictionary mapping tool names to their paths
        """
        return {
            "gcc": self.get_gcc_path(),
            "g++": self.get_gxx_path(),
            "ar": self.get_ar_path(),
            "objcopy": self.get_objcopy_path(),
            "size": self.get_size_path(),
            "objdump": self.get_objdump_path(),
        }

    def get_bin_path(self) -> Optional[Path]:
        """Get path to toolchain bin directory.

        Returns:
            Path to bin directory or None if not found
        """
        return self.get_bin_dir()

    def verify_installation(self) -> bool:
        """Verify that the toolchain is properly installed.

        Returns:
            True if all essential binaries are present

        Raises:
            ESP32ToolchainError: If essential binaries are missing
        """
        required_tools = ["gcc", "g++", "ar", "objcopy"]
        missing_tools = []

        for tool in required_tools:
            path = self._find_binary(tool)
            if path is None or not path.exists():
                missing_tools.append(tool)

        if missing_tools:
            raise ESP32ToolchainError(
                f"Toolchain installation incomplete. Missing binaries: {', '.join(missing_tools)}"
            )

        return True

    def get_toolchain_info(self) -> Dict[str, Any]:
        """Get information about the installed toolchain.

        Returns:
            Dictionary with toolchain information
        """
        info = {
            "type": self.toolchain_type,
            "version": self.version,
            "path": str(self.toolchain_path),
            "url": self.toolchain_url,
            "installed": self.is_installed(),
            "binary_prefix": self.binary_prefix,
        }

        if self.is_installed():
            info["bin_dir"] = str(self.get_bin_dir())
            info["tools"] = {
                name: str(path) if path else None
                for name, path in self.get_all_tool_paths().items()
            }

        return info
