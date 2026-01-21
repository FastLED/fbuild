"""Toolchain management for AVR-GCC.

This module handles downloading, extracting, and managing the AVR-GCC
toolchain required for building Arduino sketches.
"""

import sys
from pathlib import Path
from typing import Any, Dict, Optional

from .cache import Cache
from .downloader import PackageDownloader
from .package import IToolchain, PackageError
from .platform_utils import PlatformDetector, PlatformError


def _safe_print(message: str) -> None:
    """Print message, ignoring errors if stdout is closed (e.g., in tests)."""
    try:
        print(message)
    except (ValueError, OSError):
        pass


class ToolchainError(PackageError):
    """Raised when toolchain operations fail."""

    pass


class ToolchainAVR(IToolchain):
    """Manages AVR-GCC toolchain."""

    # AVR-GCC version used by Arduino
    VERSION = "7.3.0-atmel3.6.1-arduino7"

    # Base URL for toolchain downloads
    BASE_URL = "https://downloads.arduino.cc/tools"

    # Platform-specific toolchain packages
    PACKAGES = {
        "windows": {
            "x86_64": "avr-gcc-7.3.0-atmel3.6.1-arduino7-i686-w64-mingw32.zip",
            "checksum": "a54f64755fff4cb792a1495e5defdd789902a2a3503982e81b898299cf39800e",
        },
        "linux": {
            "x86_64": "avr-gcc-7.3.0-atmel3.6.1-arduino7-x86_64-pc-linux-gnu.tar.bz2",
            "checksum": "bd8c37f6952a2130ac9ee32c53f6a660feb79bee8353c8e289eb60fdcefed91e",
            "i686": "avr-gcc-7.3.0-atmel3.6.1-arduino7-i686-pc-linux-gnu.tar.bz2",
            "aarch64": "avr-gcc-7.3.0-atmel3.6.1-arduino7-aarch64-pc-linux-gnu.tar.bz2",
            "armv7l": "avr-gcc-7.3.0-atmel3.6.1-arduino7-arm-linux-gnueabihf.tar.bz2",
        },
        "darwin": {
            "x86_64": "avr-gcc-7.3.0-atmel3.6.1-arduino7-x86_64-apple-darwin14.tar.bz2",
            "checksum": "4c9ca2d87b5c1b5c82f567a9bfc0fdaef57fe8b9f74bae1e32b3e1964612d85e",
        },
    }

    # Required tools (executables in bin/)
    REQUIRED_TOOLS = [
        "avr-gcc",
        "avr-g++",
        "avr-ar",
        "avr-objcopy",
        "avr-size",
        "avr-nm",
        "avr-objdump",
        "avr-ranlib",
        "avr-strip",
    ]

    # Required subdirectories (libs and headers)
    REQUIRED_DIRS = [
        "bin",  # Executables
        "avr/include",  # AVR C library headers
        "lib/gcc/avr",  # GCC AVR libraries
    ]

    # Key header files that must exist
    REQUIRED_HEADERS = [
        "avr/include/avr/io.h",
        "avr/include/avr/interrupt.h",
        "avr/include/stdio.h",
        "avr/include/stdlib.h",
        "avr/include/string.h",
    ]

    # Key library files patterns
    REQUIRED_LIB_PATTERNS = [
        "lib/gcc/avr/*/libgcc.a",
        "avr/lib/libc.a",
        "avr/lib/libm.a",
    ]

    def __init__(self, cache: Cache):
        """Initialize toolchain manager.

        Args:
            cache: Cache instance for storing toolchain
        """
        self.cache = cache
        self.downloader = PackageDownloader()
        self._toolchain_path: Optional[Path] = None

    @staticmethod
    def detect_platform() -> tuple[str, str]:
        """Detect host platform and architecture.

        Returns:
            Tuple of (platform, architecture)
            Platform: 'windows', 'linux', or 'darwin'
            Architecture: 'x86_64', 'i686', 'aarch64', 'armv7l'

        Raises:
            ToolchainError: If platform is not supported
        """
        try:
            return PlatformDetector.detect_avr_platform()
        except PlatformError as e:
            raise ToolchainError(str(e))

    def _get_package_details(self) -> tuple[str, Optional[str]]:
        """Get package filename and checksum for current platform.

        Returns:
            Tuple of (package_filename, checksum)

        Raises:
            ToolchainError: If no package available for platform
        """
        plat, arch = self.detect_platform()

        if plat not in self.PACKAGES:
            raise ToolchainError(f"No toolchain package for platform: {plat}")

        platform_packages = self.PACKAGES[plat]

        # For Windows and macOS, only x86_64 is available
        if plat in ("windows", "darwin"):
            if arch != "x86_64":
                # Try to use x86_64 package anyway
                arch = "x86_64"

        if arch not in platform_packages:
            # Try x86_64 as fallback
            if "x86_64" in platform_packages:
                arch = "x86_64"
            else:
                raise ToolchainError(f"No toolchain package for {plat}/{arch}. " + f"Available: {list(platform_packages.keys())}")

        package_name = platform_packages[arch]
        checksum = platform_packages.get("checksum")

        return package_name, checksum

    def ensure_toolchain(self, force_download: bool = False) -> Path:
        """Ensure toolchain is available, downloading if necessary.

        Args:
            force_download: Force re-download even if cached

        Returns:
            Path to toolchain root directory

        Raises:
            ToolchainError: If toolchain cannot be obtained or verified
        """
        # Check if already loaded
        if self._toolchain_path and not force_download:
            return self._toolchain_path

        # Get package info
        package_name, checksum = self._get_package_details()

        # Use URL and version for cache path
        url = f"{self.BASE_URL}/{package_name}"
        toolchain_path = self.cache.get_toolchain_path(self.BASE_URL, self.VERSION)
        package_path = self.cache.get_package_path(self.BASE_URL, self.VERSION, package_name)

        # Check if already extracted and verified
        if not force_download and self.cache.is_toolchain_cached(self.BASE_URL, self.VERSION):
            # Comprehensive verification
            if self._verify_toolchain(toolchain_path):
                self._toolchain_path = toolchain_path
                return toolchain_path
            else:
                _safe_print("Cached toolchain failed validation, re-downloading...")

        # Need to download and extract
        self.cache.ensure_directories()

        _safe_print(f"Downloading AVR-GCC toolchain ({self.VERSION})...")

        try:
            # Ensure package directory exists
            package_path.parent.mkdir(parents=True, exist_ok=True)

            # Download if not cached
            if force_download or not package_path.exists():
                self.downloader.download(url, package_path, checksum)
            else:
                _safe_print(f"Using cached {package_name}")

            # Extract
            _safe_print("Extracting toolchain...")
            toolchain_path.parent.mkdir(parents=True, exist_ok=True)

            # Extract to a temporary location first
            import shutil
            import tempfile

            with tempfile.TemporaryDirectory() as temp_dir:
                temp_path = Path(temp_dir)
                self.downloader.extract_archive(package_path, temp_path, show_progress=False)

                # Find the actual toolchain directory (may be nested)
                extracted_dirs = list(temp_path.iterdir())
                if len(extracted_dirs) == 1 and extracted_dirs[0].is_dir():
                    # Single directory extracted, use it
                    src_dir = extracted_dirs[0]
                else:
                    # Multiple items extracted, use the temp dir itself
                    src_dir = temp_path

                # Move to final location
                if toolchain_path.exists():
                    shutil.rmtree(toolchain_path)
                shutil.move(str(src_dir), str(toolchain_path))

            # Comprehensive verification
            if not self._verify_toolchain(toolchain_path):
                raise ToolchainError("Toolchain verification failed after extraction")

            self._toolchain_path = toolchain_path
            _safe_print(f"Toolchain ready at {toolchain_path}")
            return toolchain_path

        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise ToolchainError(f"Failed to setup toolchain: {e}")

    def _verify_toolchain(self, toolchain_path: Path) -> bool:
        """Comprehensively verify toolchain installation.

        Checks for:
        - All required executables in bin/
        - Required directories (lib, include, etc.)
        - Key header files
        - Key library files

        Args:
            toolchain_path: Path to toolchain directory

        Returns:
            True if toolchain is complete and valid
        """
        # Check required directories
        for dir_path in self.REQUIRED_DIRS:
            if not (toolchain_path / dir_path).exists():
                _safe_print(f"Missing directory: {dir_path}")
                return False

        # Check required executables
        bin_dir = toolchain_path / "bin"
        exe_suffix = ".exe" if sys.platform == "win32" else ""

        for tool in self.REQUIRED_TOOLS:
            tool_path = bin_dir / f"{tool}{exe_suffix}"
            if not tool_path.exists():
                _safe_print(f"Missing tool: {tool}")
                return False

        # Check required headers
        for header in self.REQUIRED_HEADERS:
            header_path = toolchain_path / header
            if not header_path.exists():
                _safe_print(f"Missing header: {header}")
                return False

        # Check required libraries (using glob patterns)
        for lib_pattern in self.REQUIRED_LIB_PATTERNS:
            lib_paths = list(toolchain_path.glob(lib_pattern))
            if not lib_paths:
                _safe_print(f"Missing library matching: {lib_pattern}")
                return False

        return True

    def get_tool_path(self, tool_name: str) -> Path:
        """Get path to a specific tool.

        Args:
            tool_name: Name of the tool (e.g., 'avr-gcc')

        Returns:
            Path to the tool executable

        Raises:
            ToolchainError: If toolchain not initialized or tool not found
        """
        if not self._toolchain_path:
            raise ToolchainError("Toolchain not initialized. Call ensure_toolchain() first.")

        exe_suffix = ".exe" if sys.platform == "win32" else ""
        tool_path = self._toolchain_path / "bin" / f"{tool_name}{exe_suffix}"

        if not tool_path.exists():
            raise ToolchainError(f"Tool not found: {tool_name}")

        return tool_path

    def get_all_tools(self) -> Dict[str, Path]:
        """Get paths to all required tools.

        Returns:
            Dictionary mapping tool names to their paths

        Raises:
            ToolchainError: If toolchain not initialized
        """
        if not self._toolchain_path:
            raise ToolchainError("Toolchain not initialized. Call ensure_toolchain() first.")

        return {tool: self.get_tool_path(tool) for tool in self.REQUIRED_TOOLS}

    # Implement BaseToolchain interface
    def get_gcc_path(self) -> Optional[Path]:
        """Get path to GCC compiler.

        Returns:
            Path to gcc binary or None if not found
        """
        try:
            return self.get_tool_path("avr-gcc")
        except ToolchainError:
            return None

    def get_gxx_path(self) -> Optional[Path]:
        """Get path to G++ compiler.

        Returns:
            Path to g++ binary or None if not found
        """
        try:
            return self.get_tool_path("avr-g++")
        except ToolchainError:
            return None

    def get_ar_path(self) -> Optional[Path]:
        """Get path to archiver (ar).

        Returns:
            Path to ar binary or None if not found
        """
        try:
            return self.get_tool_path("avr-ar")
        except ToolchainError:
            return None

    def get_objcopy_path(self) -> Optional[Path]:
        """Get path to objcopy utility.

        Returns:
            Path to objcopy binary or None if not found
        """
        try:
            return self.get_tool_path("avr-objcopy")
        except ToolchainError:
            return None

    def get_size_path(self) -> Optional[Path]:
        """Get path to size utility.

        Returns:
            Path to size binary or None if not found
        """
        try:
            return self.get_tool_path("avr-size")
        except ToolchainError:
            return None

    def get_bin_dir(self) -> Optional[Path]:
        """Get path to toolchain bin directory.

        Returns:
            Path to bin directory containing compiler binaries
        """
        if not self._toolchain_path:
            return None
        return self._toolchain_path / "bin"

    def is_installed(self) -> bool:
        """Check if toolchain is already installed.

        Returns:
            True if toolchain directory exists and is valid
        """
        if not self._toolchain_path:
            # Try to get from cache
            toolchain_path = self.cache.get_toolchain_path(self.BASE_URL, self.VERSION)
            if not toolchain_path.exists():
                return False
            # Verify it
            if self._verify_toolchain(toolchain_path):
                self._toolchain_path = toolchain_path
                return True
            return False
        return self._verify_toolchain(self._toolchain_path)

    # Implement BasePackage interface
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
        info = {
            "type": "toolchain",
            "platform": "avr",
            "version": self.VERSION,
            "base_url": self.BASE_URL,
            "installed": self._toolchain_path is not None,
        }

        if self._toolchain_path:
            info["path"] = str(self._toolchain_path)
            info["tools"] = {name: str(path) for name, path in self.get_all_tools().items()}

        return info
