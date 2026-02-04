"""RP2040/RP2350 Platform Package Management.

This module coordinates RP2040/RP2350 platform components including toolchain and framework.
It provides a unified interface for managing Raspberry Pi Pico builds.

Platform Components:
    - ARM GCC Toolchain (arm-none-eabi-gcc)
    - arduino-pico Framework (Arduino core for RP2040/RP2350)

Supported Boards:
    - Raspberry Pi Pico (RP2040, ARM Cortex-M0+ @ 133MHz)
    - Raspberry Pi Pico W (RP2040 with WiFi, ARM Cortex-M0+ @ 133MHz)
    - Raspberry Pi Pico 2 (RP2350, ARM Cortex-M33 @ 150MHz)
    - Raspberry Pi Pico 2 W (RP2350 with WiFi, ARM Cortex-M33 @ 150MHz)
"""

from pathlib import Path
from typing import Any, Dict, List

from .cache import Cache
from .framework_rp2040 import FrameworkErrorRP2040, FrameworkRP2040
from .package import IPackage, PackageError
from .toolchain_rp2040 import ToolchainErrorRP2040, ToolchainRP2040


class PlatformErrorRP2040(PackageError):
    """Raised when RP2040/RP2350 platform operations fail."""

    pass


class PlatformRP2040(IPackage):
    """Manages RP2040/RP2350 platform components and configuration.

    This class coordinates the ARM GCC toolchain and arduino-pico framework to provide
    a complete build environment for Raspberry Pi Pico boards.
    """

    def __init__(self, cache: Cache, board_mcu: str, show_progress: bool = True):
        """Initialize RP2040/RP2350 platform manager.

        Args:
            cache: Cache manager instance
            board_mcu: MCU type (e.g., "rp2040", "rp2350")
            show_progress: Whether to show download/extraction progress
        """
        self.cache = cache
        self.board_mcu = board_mcu
        self.show_progress = show_progress

        # Initialize toolchain and framework
        self.toolchain = ToolchainRP2040(cache, show_progress=show_progress)
        self.framework = FrameworkRP2040(cache, show_progress=show_progress)

    def ensure_package(self) -> Path:
        """Ensure platform components are downloaded and extracted.

        Returns:
            Path to the framework directory (main platform directory)

        Raises:
            PlatformErrorRP2040: If download or extraction fails
        """
        try:
            # Ensure toolchain is installed
            self.toolchain.ensure_toolchain()

            # Ensure framework is installed
            framework_path = self.framework.ensure_framework()

            return framework_path

        except (ToolchainErrorRP2040, FrameworkErrorRP2040) as e:
            raise PlatformErrorRP2040(f"Failed to install RP2040/RP2350 platform: {e}")
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise PlatformErrorRP2040(f"Unexpected error installing platform: {e}")

    def is_installed(self) -> bool:
        """Check if platform is already installed.

        Returns:
            True if both toolchain and framework are installed
        """
        return self.toolchain.is_installed() and self.framework.is_installed()

    def get_include_dirs(self, board_config: Any) -> List[Path]:
        """Get include directories for RP2040/RP2350 builds.

        Args:
            board_config: Board configuration object

        Returns:
            List of include directory paths
        """
        includes = []

        # Core includes
        try:
            core_includes = self.framework.get_core_includes("rp2040")
            includes.extend(core_includes)
        except FrameworkErrorRP2040:
            pass

        # Variant includes (if board_config has variant info)
        if hasattr(board_config, "variant"):
            variant_dir = self.framework.get_variant_dir(board_config.variant)
            if variant_dir:
                includes.append(variant_dir)

        return includes

    def get_core_sources(self) -> List[Path]:
        """Get core source files for RP2040/RP2350 builds.

        Returns:
            List of core source file paths
        """
        try:
            return self.framework.get_core_sources("rp2040")
        except FrameworkErrorRP2040:
            return []

    def get_toolchain_binaries(self) -> Dict[str, Path]:
        """Get paths to toolchain binaries.

        Returns:
            Dictionary mapping tool names to paths

        Raises:
            PlatformErrorRP2040: If toolchain binaries are not found
        """
        tools = self.toolchain.get_all_tool_paths()

        # Verify all required tools exist
        required_tools = ["gcc", "g++", "ar", "objcopy", "size"]
        for tool_name in required_tools:
            if tool_name not in tools or tools[tool_name] is None:
                raise PlatformErrorRP2040(f"Required tool not found: {tool_name}")

        # Filter out None values
        return {name: path for name, path in tools.items() if path is not None}

    def get_package_info(self) -> Dict[str, Any]:
        """Get information about the installed platform.

        Returns:
            Dictionary with platform information
        """
        return self.get_platform_info()

    def get_board_json(self, board_id: str) -> Dict[str, Any]:
        """Get board configuration in JSON format.

        This method returns board configuration compatible with the format
        expected by ConfigurableCompiler and ConfigurableLinker.

        Args:
            board_id: Board identifier (e.g., "rpipico", "rpipico2")

        Returns:
            Dictionary containing board configuration

        Raises:
            PlatformErrorRP2040: If board is not supported
        """
        from .. import platform_configs

        config = platform_configs.load_board_config(board_id)
        if config is None:
            available = [c for c in platform_configs.list_available_configs() if c.startswith("rpi")]
            raise PlatformErrorRP2040(f"Unsupported board: {board_id}. Available: {', '.join(available)}")

        # Transform to expected format for ConfigurableCompiler/Linker - type-safe access
        return {
            "build": {
                "mcu": config.mcu,
                "f_cpu": config.f_cpu,
                "core": "rp2040",
                "variant": config.variant,
                "board": config.defines[-1] if config.defines else "",
            },
            "name": config.name,
            "upload": config.upload,
        }

    def get_platform_info(self) -> Dict[str, Any]:
        """Get information about the installed platform.

        Returns:
            Dictionary with platform information
        """
        info = {
            "platform": "raspberrypi",
            "mcu": self.board_mcu,
            "installed": self.is_installed(),
            "toolchain": self.toolchain.get_toolchain_info(),
            "framework": self.framework.get_framework_info(),
        }

        return info
