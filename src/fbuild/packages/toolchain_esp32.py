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
    - RISC-V: ESP32-C3, ESP32-C6, ESP32-H2, ESP32-C2, ESP32-C5, ESP32-P4
    - Xtensa: ESP32, ESP32-S2, ESP32-S3
"""

from pathlib import Path
from typing import Any, Dict, Literal, Optional, cast

from .cache import Cache
from .downloader import DownloadError, ExtractionError, PackageDownloader
from .package import IToolchain, PackageError
from .platform_utils import PlatformDetector
from .toolchain_binaries import ToolchainBinaryFinder
from .toolchain_metadata import MetadataParseError, ToolchainMetadataParser


class ToolchainErrorESP32(PackageError):
    """Raised when ESP32 toolchain operations fail."""

    pass


ToolchainType = Literal["riscv32-esp", "xtensa-esp-elf"]


class ToolchainESP32(IToolchain):
    """Manages ESP32 toolchain download, extraction, and access.

    This class handles downloading and managing GCC toolchains for ESP32 family:
    - RISC-V GCC for ESP32-C3, C6, H2, C2, C5, P4 chips
    - Xtensa GCC for ESP32, S2, S3 chips
    """

    # Toolchain name mappings (FALLBACK ONLY - used when binary discovery fails)
    # These values are overridden by actual binary discovery after installation
    TOOLCHAIN_NAMES = {
        "riscv32-esp": "riscv32-esp-elf",
        "xtensa-esp-elf": "xtensa-esp32-elf",  # NOTE: tools.json says "xtensa-esp-elf" but binaries are "xtensa-esp32-elf"
    }

    # MCU to toolchain type mapping
    MCU_TOOLCHAIN_MAP = {
        "esp32": "xtensa-esp-elf",
        "esp32s2": "xtensa-esp-elf",
        "esp32s3": "xtensa-esp-elf",
        "esp32c2": "riscv32-esp",
        "esp32c3": "riscv32-esp",
        "esp32c5": "riscv32-esp",
        "esp32c6": "riscv32-esp",
        "esp32h2": "riscv32-esp",
        "esp32p4": "riscv32-esp",
    }

    # Xtensa MCUs that have MCU-specific wrapper binaries in the unified toolchain.
    # The wrapper binaries (e.g., xtensa-esp32-elf-gcc) automatically handle
    # -mdynconfig for correct endianness. Without them, the generic xtensa-esp-elf-gcc
    # defaults to big-endian and linking fails with "failed to merge target specific data".
    XTENSA_MCUS = frozenset({"esp32", "esp32s2", "esp32s3"})

    def __init__(
        self,
        cache: Cache,
        toolchain_url: str,
        toolchain_type: ToolchainType,
        show_progress: bool = True,
        mcu: Optional[str] = None,
    ):
        """Initialize ESP32 toolchain manager.

        Args:
            cache: Cache manager instance
            toolchain_url: URL to toolchain package (e.g., GitHub release ZIP)
            toolchain_type: Type of toolchain ("riscv32-esp" or "xtensa-esp-elf")
            show_progress: Whether to show download/extraction progress
            mcu: Target MCU (e.g., "esp32", "esp32s3"). For Xtensa toolchains,
                 this selects the MCU-specific wrapper binaries that auto-handle
                 endianness configuration.
        """
        self.cache = cache
        self.toolchain_url = toolchain_url
        self.toolchain_type = toolchain_type
        self.show_progress = show_progress
        self.mcu = mcu
        self.downloader = PackageDownloader()

        # Extract version from URL
        self.version = self._extract_version_from_url(toolchain_url)

        # Get toolchain path from cache
        self.toolchain_path = cache.get_toolchain_path(toolchain_url, self.version)

        # Initialize binary finder with fallback prefix
        fallback_prefix = self.TOOLCHAIN_NAMES.get(toolchain_type, toolchain_type)

        # Initialize utilities
        self.metadata_parser = ToolchainMetadataParser(self.downloader)
        self.binary_finder = ToolchainBinaryFinder(self.toolchain_path, fallback_prefix)

        # Cache for discovered binary prefix (set after ensure_toolchain)
        self._discovered_prefix: Optional[str] = None

        # Set the actual binary prefix (will use discovery if toolchain is installed)
        self.binary_prefix = self._get_binary_prefix()

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

    def _get_expected_binary_name(self) -> Optional[str]:
        """Get the expected GCC binary name from tools.json manifest.

        Returns:
            Expected binary name (e.g., "xtensa-esp-elf-gcc"), or None if not found
        """
        tools_json_path = self.toolchain_path / "tools.json"
        if not tools_json_path.exists():
            return None

        toolchain_name = f"toolchain-{self.toolchain_type}"
        return self.metadata_parser.get_expected_binary_name(tools_json_path, toolchain_name)

    def _get_binary_prefix(self) -> str:
        """Get the binary prefix, using MCU-specific wrapper for Xtensa toolchains.

        For Xtensa toolchains (esp32, esp32s2, esp32s3), the unified toolchain
        provides MCU-specific wrapper binaries (e.g., xtensa-esp32-elf-gcc) that
        automatically handle the -mdynconfig flag for correct endianness. Using the
        generic xtensa-esp-elf-gcc would default to big-endian, causing link failures.

        Returns:
            Binary prefix string (e.g., "xtensa-esp32-elf", "riscv32-esp-elf")
        """
        # Return cached discovered prefix if available
        if self._discovered_prefix is not None:
            return self._discovered_prefix

        # For Xtensa toolchains with a known MCU, use the MCU-specific wrapper prefix.
        # These wrappers auto-add -mdynconfig for correct endianness and multilib selection.
        if self.mcu and self.toolchain_type == "xtensa-esp-elf" and self.mcu in self.XTENSA_MCUS:
            mcu_prefix = f"xtensa-{self.mcu}-elf"
            # Verify the MCU-specific binary exists before committing
            if self.toolchain_path.exists():
                temp_finder = ToolchainBinaryFinder(self.toolchain_path, mcu_prefix)
                if temp_finder.verify_binary_exists("gcc"):
                    self._discovered_prefix = mcu_prefix
                    self.binary_finder.binary_prefix = mcu_prefix
                    if self.show_progress:
                        print(f"Using MCU-specific toolchain prefix: {mcu_prefix}")
                    return mcu_prefix

        # Try to discover from installed binaries (check toolchain_path exists)
        if self.toolchain_path.exists():
            expected_binary = self._get_expected_binary_name()
            discovered = self.binary_finder.discover_binary_prefix(
                verbose=self.show_progress,
                expected_binary_name=expected_binary,
            )
            if discovered:
                self._discovered_prefix = discovered
                # Update binary_finder with correct prefix
                self.binary_finder.binary_prefix = discovered
                return discovered

        # Fallback to hardcoded mapping
        return self.TOOLCHAIN_NAMES.get(self.toolchain_type, self.toolchain_type)

    def _update_binary_prefix_after_install(self) -> None:
        """Update binary prefix after toolchain installation.

        Called by ensure_toolchain() after successful installation.
        For Xtensa toolchains with a known MCU, uses MCU-specific wrapper prefix.
        Falls back to tools.json manifest discovery.
        """
        # For Xtensa toolchains with a known MCU, prefer MCU-specific wrapper binaries
        if self.mcu and self.toolchain_type == "xtensa-esp-elf" and self.mcu in self.XTENSA_MCUS:
            mcu_prefix = f"xtensa-{self.mcu}-elf"
            temp_finder = ToolchainBinaryFinder(self.toolchain_path, mcu_prefix)
            if temp_finder.verify_binary_exists("gcc"):
                self._discovered_prefix = mcu_prefix
                self.binary_finder.binary_prefix = mcu_prefix
                self.binary_prefix = mcu_prefix
                if self.show_progress:
                    print(f"Using MCU-specific toolchain prefix: {mcu_prefix}")
                return

        expected_binary = self._get_expected_binary_name()
        discovered = self.binary_finder.discover_binary_prefix(
            verbose=self.show_progress,
            expected_binary_name=expected_binary,
        )
        if discovered:
            self._discovered_prefix = discovered
            self.binary_finder.binary_prefix = discovered
            self.binary_prefix = discovered
            if self.show_progress:
                print(f"Discovered toolchain binary prefix: {discovered}")

    @staticmethod
    def get_toolchain_type_for_mcu(mcu: str) -> ToolchainType:
        """Get the toolchain type needed for a specific MCU.

        Args:
            mcu: MCU type (e.g., "esp32c6", "esp32s3", "esp32")

        Returns:
            Toolchain type string ("riscv32-esp" or "xtensa-esp-elf")

        Raises:
            ToolchainErrorESP32: If MCU type is unknown
        """
        mcu_lower = mcu.lower()
        if mcu_lower in ToolchainESP32.MCU_TOOLCHAIN_MAP:
            return cast(ToolchainType, ToolchainESP32.MCU_TOOLCHAIN_MAP[mcu_lower])

        raise ToolchainErrorESP32(f"Unknown MCU type: {mcu}")

    @staticmethod
    def detect_platform() -> str:
        """Detect the current platform for toolchain selection.

        Returns:
            Platform identifier (win32, win64, linux-amd64, linux-arm64, macos, macos-arm64)
        """
        try:
            return PlatformDetector.detect_esp32_platform()
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise ToolchainErrorESP32(str(e))

    def _get_platform_url_from_metadata(self) -> str:
        """Download metadata package and extract platform-specific toolchain URL.

        Returns:
            URL to platform-specific toolchain archive

        Raises:
            ToolchainErrorESP32: If metadata cannot be parsed or platform not found
        """
        try:
            current_platform = self.detect_platform()
            toolchain_name = f"toolchain-{self.toolchain_type}"

            return self.metadata_parser.get_platform_url(
                metadata_url=self.toolchain_url,
                metadata_path=self.toolchain_path,
                toolchain_name=toolchain_name,
                platform=current_platform,
                show_progress=self.show_progress,
            )
        except MetadataParseError as e:
            raise ToolchainErrorESP32(str(e))

    def ensure_toolchain(self) -> Path:
        """Ensure toolchain is downloaded and extracted.

        Returns:
            Path to the extracted toolchain directory

        Raises:
            ToolchainErrorESP32: If download or extraction fails
        """
        if self.is_installed():
            # Update binary prefix if not already discovered
            if self._discovered_prefix is None:
                self._update_binary_prefix_after_install()

            if self.show_progress:
                print(f"Using cached {self.toolchain_type} toolchain {self.version}")
            return self.toolchain_path

        try:
            # Step 1: Get platform-specific URL from metadata
            platform_url = self._get_platform_url_from_metadata()

            if self.show_progress:
                print(f"Downloading {self.toolchain_type} toolchain for {self.detect_platform()}...")

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
                self.downloader.download(platform_url, archive_path, show_progress=self.show_progress)
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

            # Use temp_extract directly as the source directory.
            # The downloader's extract_archive() already strips single top-level
            # directories (e.g., xtensa-esp-elf/ → contents moved to temp_extract/).
            # After stripping, temp_extract contains bin/, lib/, include/, etc. directly.
            # NOTE: Do NOT use glob("*esp*") here - it matches the target sysroot
            # subdirectory (xtensa-esp-elf/) instead of the full toolchain contents,
            # causing only unprefixed binutils to be installed (no gcc/g++/ar).
            source_dir = temp_extract

            # Move to final location (toolchain_path/bin)
            final_bin_path = toolchain_cache_dir
            if final_bin_path.exists() and final_bin_path != temp_extract:
                # Remove old installation
                import shutil

                for item in final_bin_path.iterdir():
                    # Skip temp files and archives that might be locked by antivirus
                    if item.name != "temp_extract" and not item.name.endswith((".zip", ".tar", ".xz", ".gz", ".download", ".tmp")):
                        try:
                            if item.is_dir():
                                shutil.rmtree(item)
                            else:
                                item.unlink()
                        except (PermissionError, OSError):
                            # Ignore errors removing old files (might be locked by antivirus)
                            pass

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

            # Discover actual binary prefix from installed files
            self._update_binary_prefix_after_install()

            # Create manifest for cache management
            from fbuild.packages.downloader import create_package_manifest

            metadata_dict: dict[str, str] = {"architecture": self.toolchain_type}
            if self.mcu:
                metadata_dict["mcu"] = self.mcu

            create_package_manifest(
                install_path=toolchain_cache_dir,
                name=f"{self.toolchain_type.upper()} Toolchain",
                package_type="toolchains",
                version=self.version,
                url=self.toolchain_url,
                metadata=metadata_dict,
            )

            return self.toolchain_path

        except (DownloadError, ExtractionError) as e:
            raise ToolchainErrorESP32(f"Failed to install {self.toolchain_type} toolchain: {e}")
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise ToolchainErrorESP32(f"Unexpected error installing toolchain: {e}")

    def is_installed(self) -> bool:
        """Check if toolchain is already installed.

        Returns:
            True if toolchain directory exists with key binaries
        """
        if not self.toolchain_path.exists():
            return False

        # Verify essential toolchain binaries exist
        return self.binary_finder.verify_binary_exists("gcc")

    def get_bin_dir(self) -> Optional[Path]:
        """Get path to toolchain bin directory.

        Returns:
            Path to bin directory containing compiler binaries, or None if not found
        """
        return self.binary_finder.find_bin_dir()

    def _find_binary(self, binary_name: str) -> Optional[Path]:
        """Find a binary in the toolchain bin directory.

        Args:
            binary_name: Name of the binary (e.g., "gcc", "g++")

        Returns:
            Path to binary or None if not found
        """
        return self.binary_finder.find_binary(binary_name)

    def get_gcc_path(self) -> Optional[Path]:
        """Get path to GCC compiler.

        Returns:
            Path to gcc binary or None if not found
        """
        return self.binary_finder.get_gcc_path()

    def get_gxx_path(self) -> Optional[Path]:
        """Get path to G++ compiler.

        Returns:
            Path to g++ binary or None if not found
        """
        return self.binary_finder.get_gxx_path()

    def get_ar_path(self) -> Optional[Path]:
        """Get path to archiver (ar).

        Returns:
            Path to ar binary or None if not found
        """
        return self.binary_finder.get_ar_path()

    def get_objcopy_path(self) -> Optional[Path]:
        """Get path to objcopy utility.

        Returns:
            Path to objcopy binary or None if not found
        """
        return self.binary_finder.get_objcopy_path()

    def get_size_path(self) -> Optional[Path]:
        """Get path to size utility.

        Returns:
            Path to size binary or None if not found
        """
        return self.binary_finder.get_size_path()

    def get_objdump_path(self) -> Optional[Path]:
        """Get path to objdump utility.

        Returns:
            Path to objdump binary or None if not found
        """
        return self.binary_finder.get_objdump_path()

    def get_gcc_ar_path(self) -> Optional[Path]:
        """Get path to gcc-ar (LTO-aware archiver).

        gcc-ar is a wrapper around ar that works with LTO bytecode objects.
        It ensures proper symbol table generation for archives containing
        objects compiled with -flto -fno-fat-lto-objects.

        Returns:
            Path to gcc-ar binary or None if not found
        """
        return self.binary_finder.get_gcc_ar_path()

    def get_gcc_ranlib_path(self) -> Optional[Path]:
        """Get path to gcc-ranlib (LTO-aware ranlib).

        gcc-ranlib is a wrapper around ranlib that works with LTO bytecode objects.
        It updates the symbol table of archives containing LTO objects.

        Returns:
            Path to gcc-ranlib binary or None if not found
        """
        return self.binary_finder.get_gcc_ranlib_path()

    def get_all_tool_paths(self) -> Dict[str, Optional[Path]]:
        """Get paths to all common toolchain binaries.

        Returns:
            Dictionary mapping tool names to their paths
        """
        return self.binary_finder.get_common_tool_paths()

    def get_all_tools(self) -> Dict[str, Path]:
        """Get paths to all required tools.

        Returns:
            Dictionary mapping tool names to their paths

        Raises:
            ToolchainErrorESP32: If any required tool is not found
        """
        tools = self.get_all_tool_paths()

        # Filter out None values and verify all tools exist
        result = {}
        for name, path in tools.items():
            if path is None:
                raise ToolchainErrorESP32(f"Required tool not found: {name}")
            result[name] = path

        return result

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
            ToolchainErrorESP32: If essential binaries are missing
        """
        try:
            return self.binary_finder.verify_installation()
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise ToolchainErrorESP32(str(e))

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
            info["tools"] = {name: str(path) if path else None for name, path in self.get_all_tool_paths().items()}

        return info

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
        return self.get_toolchain_info()
