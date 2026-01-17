"""RP2040/RP2350 Toolchain Management.

This module handles downloading, extracting, and managing ARM GCC toolchain
needed for Raspberry Pi Pico (RP2040) and Pico 2 (RP2350) builds.

Toolchain Download Process:
    1. Download ARM GCC toolchain for ARM Cortex-M
    2. Extract to cache directory
    3. Provide access to compiler binaries

Toolchain Structure (after extraction):
    arm-none-eabi/
    ├── bin/
    │   ├── arm-none-eabi-gcc.exe
    │   ├── arm-none-eabi-g++.exe
    │   ├── arm-none-eabi-ar.exe
    │   ├── arm-none-eabi-objcopy.exe
    │   └── ...
    ├── lib/
    └── include/

Supported Boards:
    - Raspberry Pi Pico (RP2040, ARM Cortex-M0+ @ 133MHz)
    - Raspberry Pi Pico W (RP2040, ARM Cortex-M0+ @ 133MHz)
    - Raspberry Pi Pico 2 (RP2350, ARM Cortex-M33 @ 150MHz)
    - Raspberry Pi Pico 2 W (RP2350, ARM Cortex-M33 @ 150MHz)
"""

import platform
from pathlib import Path
from typing import Any, Dict, Optional

from .cache import Cache
from .downloader import DownloadError, ExtractionError, PackageDownloader
from .package import IToolchain, PackageError


class ToolchainErrorRP2040(PackageError):
    """Raised when RP2040/RP2350 toolchain operations fail."""

    pass


class ToolchainRP2040(IToolchain):
    """Manages RP2040/RP2350 toolchain download, extraction, and access.

    This class handles downloading and managing ARM GCC toolchain for RP2040/RP2350:
    - ARM GCC for RP2040 (ARM Cortex-M0+)
    - ARM GCC for RP2350 (ARM Cortex-M33)
    """

    # Binary prefix for ARM GCC toolchain
    BINARY_PREFIX = "arm-none-eabi-"

    # Toolchain download URLs for different platforms
    # Source: https://developer.arm.com/downloads/-/arm-gnu-toolchain-downloads
    # Version 15.2.Rel1 (released December 17, 2025)
    TOOLCHAIN_URLS = {
        "Windows": "https://developer.arm.com/-/media/Files/downloads/gnu/15.2.rel1/binrel/arm-gnu-toolchain-15.2.rel1-mingw-w64-x86_64-arm-none-eabi.zip",
        "Linux": "https://developer.arm.com/-/media/Files/downloads/gnu/15.2.rel1/binrel/arm-gnu-toolchain-15.2.rel1-x86_64-arm-none-eabi.tar.xz",
        "Darwin": "https://developer.arm.com/-/media/Files/downloads/gnu/15.2.rel1/binrel/arm-gnu-toolchain-15.2.rel1-darwin-x86_64-arm-none-eabi.tar.xz",
    }

    def __init__(
        self,
        cache: Cache,
        show_progress: bool = True,
    ):
        """Initialize RP2040/RP2350 toolchain manager.

        Args:
            cache: Cache manager instance
            show_progress: Whether to show download/extraction progress
        """
        self.cache = cache
        self.show_progress = show_progress
        self.downloader = PackageDownloader()

        # Get platform-specific toolchain URL
        self.toolchain_url = self._get_platform_toolchain_url()

        # Extract version from URL
        self.version = self._extract_version_from_url(self.toolchain_url)

        # Get toolchain path from cache
        self.toolchain_path = cache.get_toolchain_path(self.toolchain_url, self.version)

    @staticmethod
    def _get_platform_toolchain_url() -> str:
        """Get toolchain URL for the current platform.

        Returns:
            URL to platform-specific toolchain archive

        Raises:
            ToolchainErrorRP2040: If platform is not supported
        """
        system = platform.system()
        if system not in ToolchainRP2040.TOOLCHAIN_URLS:
            raise ToolchainErrorRP2040(f"Unsupported platform: {system}. " + f"Supported platforms: {', '.join(ToolchainRP2040.TOOLCHAIN_URLS.keys())}")
        return ToolchainRP2040.TOOLCHAIN_URLS[system]

    @staticmethod
    def _extract_version_from_url(url: str) -> str:
        """Extract version string from toolchain URL.

        Args:
            url: Toolchain URL

        Returns:
            Version string (e.g., "15.2.rel1")
        """
        # Extract version from URL pattern: .../15.2.rel1/...
        parts = url.split("/")
        for part in parts:
            if "rel" in part.lower() and part[0].isdigit():
                return part

        # Fallback: use URL hash if version extraction fails
        from .cache import Cache

        return Cache.hash_url(url)[:8]

    def ensure_toolchain(self) -> Path:
        """Ensure toolchain is downloaded and extracted.

        Returns:
            Path to the extracted toolchain directory

        Raises:
            ToolchainErrorRP2040: If download or extraction fails
        """
        if self.is_installed():
            if self.show_progress:
                print(f"Using cached ARM GCC toolchain {self.version}")
            return self.toolchain_path

        try:
            if self.show_progress:
                print(f"Downloading ARM GCC toolchain {self.version}...")

            # Download and extract toolchain package
            self.cache.ensure_directories()

            # Use downloader to handle download and extraction
            archive_name = Path(self.toolchain_url).name
            toolchain_cache_dir = self.toolchain_path.parent / "bin"
            toolchain_cache_dir.mkdir(parents=True, exist_ok=True)
            archive_path = toolchain_cache_dir / archive_name

            # Download if not cached
            if not archive_path.exists():
                self.downloader.download(self.toolchain_url, archive_path, show_progress=self.show_progress)
            else:
                if self.show_progress:
                    print("Using cached toolchain archive")

            # Extract to toolchain directory
            if self.show_progress:
                print("Extracting toolchain...")

            # Create temp extraction directory
            temp_extract = toolchain_cache_dir / "temp_extract"
            temp_extract.mkdir(parents=True, exist_ok=True)

            self.downloader.extract_archive(archive_path, temp_extract, show_progress=self.show_progress)

            # Find the toolchain directory in the extracted content
            # Usually it's a subdirectory like "arm-gnu-toolchain-15.2.rel1-..."
            extracted_dirs = list(temp_extract.glob("arm-gnu-toolchain-*"))
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
                    if item.name != "temp_extract" and not item.name.endswith((".zip", ".tar", ".xz", ".gz")):
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

            # Update toolchain_path to point to the actual installation location
            self.toolchain_path = toolchain_cache_dir

            if self.show_progress:
                print("ARM GCC toolchain installed successfully")

            return self.toolchain_path

        except (DownloadError, ExtractionError) as e:
            raise ToolchainErrorRP2040(f"Failed to install ARM GCC toolchain: {e}")
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise ToolchainErrorRP2040(f"Unexpected error installing toolchain: {e}")

    def is_installed(self) -> bool:
        """Check if toolchain is already installed.

        Returns:
            True if toolchain directory exists with key binaries
        """
        if not self.toolchain_path.exists():
            return False

        # Verify essential toolchain binaries exist
        gcc_path = self._find_binary("gcc")
        return gcc_path is not None and gcc_path.exists()

    def get_bin_dir(self) -> Optional[Path]:
        """Get path to toolchain bin directory.

        Returns:
            Path to bin directory containing compiler binaries, or None if not found
        """
        # Check common bin directory locations
        possible_paths = [
            self.toolchain_path / "bin" / "bin",  # Nested bin
            self.toolchain_path / "bin",  # Direct bin
        ]

        for path in possible_paths:
            if path.exists() and path.is_dir():
                # Verify it contains toolchain binaries
                if list(path.glob(f"{self.BINARY_PREFIX}gcc*")):
                    return path

        return None

    def _find_binary(self, binary_name: str) -> Optional[Path]:
        """Find a binary in the toolchain bin directory.

        Args:
            binary_name: Name of the binary (e.g., "gcc", "g++")

        Returns:
            Path to binary or None if not found
        """
        bin_dir = self.get_bin_dir()
        if not bin_dir:
            return None

        # Try with and without .exe extension
        binary_path = bin_dir / f"{self.BINARY_PREFIX}{binary_name}"
        if binary_path.exists():
            return binary_path

        binary_path_exe = bin_dir / f"{self.BINARY_PREFIX}{binary_name}.exe"
        if binary_path_exe.exists():
            return binary_path_exe

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

    def get_elf2uf2_path(self) -> Optional[Path]:
        """Get path to elf2uf2 utility (if available).

        Note: elf2uf2 is typically provided by the Pico SDK, not the toolchain.
        This method is here for API compatibility but will likely return None.

        Returns:
            Path to elf2uf2 binary or None if not found
        """
        # Check if elf2uf2 exists in the toolchain bin directory
        bin_dir = self.get_bin_dir()
        if not bin_dir:
            return None

        # Try with and without .exe extension
        elf2uf2_path = bin_dir / "elf2uf2"
        if elf2uf2_path.exists():
            return elf2uf2_path

        elf2uf2_path_exe = bin_dir / "elf2uf2.exe"
        if elf2uf2_path_exe.exists():
            return elf2uf2_path_exe

        return None

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
            "elf2uf2": self.get_elf2uf2_path(),
        }

    def get_all_tools(self) -> Dict[str, Path]:
        """Get paths to all required tools (IToolchain interface).

        Returns:
            Dictionary mapping tool names to their paths (non-None only)

        Raises:
            ToolchainErrorRP2040: If any required tool is not found
        """
        all_tools = self.get_all_tool_paths()

        # Filter out None values and verify all required tools exist
        # Note: elf2uf2 is optional, as it may be provided by the framework
        required_tools = ["gcc", "g++", "ar", "objcopy", "size"]
        result = {}
        for name, path in all_tools.items():
            if name in required_tools and path is None:
                raise ToolchainErrorRP2040(f"Required tool '{name}' not found in toolchain")
            if path is not None:
                result[name] = path

        return result

    def get_toolchain_info(self) -> Dict[str, Any]:
        """Get information about the installed toolchain.

        Returns:
            Dictionary with toolchain information
        """
        info = {
            "type": "arm-none-eabi",
            "version": self.version,
            "path": str(self.toolchain_path),
            "url": self.toolchain_url,
            "installed": self.is_installed(),
            "binary_prefix": self.BINARY_PREFIX,
        }

        if self.is_installed():
            info["bin_dir"] = str(self.get_bin_dir())
            info["tools"] = {name: str(path) if path else None for name, path in self.get_all_tool_paths().items()}

        return info

    # Implement IPackage interface
    def ensure_package(self) -> Path:
        """Ensure package is downloaded and extracted.

        Returns:
            Path to the extracted package directory

        Raises:
            PackageError: If download or extraction fails
        """
        return self.ensure_toolchain()

    def get_package_info(self) -> Dict[str, Any]:
        """Get information about the package.

        Returns:
            Dictionary with package metadata (version, path, etc.)
        """
        return self.get_toolchain_info()
