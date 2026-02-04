"""Teensy Platform Package Management.

This module coordinates Teensy platform components including toolchain and framework.
It provides a unified interface for managing Teensy 4.x platform builds.

Platform Components:
    - ARM GCC Toolchain (arm-none-eabi-gcc)
    - Teensy Cores Framework (Arduino core for Teensy 4.x)

Supported Boards:
    - Teensy 4.1 (NXP i.MX RT1062, ARM Cortex-M7 @ 600MHz)
    - Teensy 4.0 (NXP i.MX RT1062, ARM Cortex-M7 @ 600MHz)
"""

from pathlib import Path
from typing import Any, Dict, List

from .cache import Cache
from .framework_teensy import FrameworkErrorTeensy, FrameworkTeensy
from .package import IPackage, PackageError
from .toolchain_teensy import ToolchainErrorTeensy, ToolchainTeensy


class PlatformErrorTeensy(PackageError):
    """Raised when Teensy platform operations fail."""

    pass


class PlatformTeensy(IPackage):
    """Manages Teensy platform components and configuration.

    This class coordinates the Teensy toolchain and framework to provide
    a complete build environment for Teensy 4.x boards.
    """

    def __init__(self, cache: Cache, board_mcu: str, show_progress: bool = True):
        """Initialize Teensy platform manager.

        Args:
            cache: Cache manager instance
            board_mcu: MCU type (e.g., "imxrt1062")
            show_progress: Whether to show download/extraction progress
        """
        self.cache = cache
        self.board_mcu = board_mcu
        self.show_progress = show_progress

        # Initialize toolchain and framework
        self.toolchain = ToolchainTeensy(cache, show_progress=show_progress)
        self.framework = FrameworkTeensy(cache, show_progress=show_progress)

    def ensure_package(self) -> Path:
        """Ensure platform components are downloaded and extracted.

        Returns:
            Path to the framework directory (main platform directory)

        Raises:
            PlatformErrorTeensy: If download or extraction fails
        """
        try:
            # Ensure toolchain is installed
            self.toolchain.ensure_toolchain()

            # Ensure framework is installed
            framework_path = self.framework.ensure_framework()

            return framework_path

        except (ToolchainErrorTeensy, FrameworkErrorTeensy) as e:
            raise PlatformErrorTeensy(f"Failed to install Teensy platform: {e}")
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise PlatformErrorTeensy(f"Unexpected error installing platform: {e}")

    def is_installed(self) -> bool:
        """Check if platform is already installed.

        Returns:
            True if both toolchain and framework are installed
        """
        return self.toolchain.is_installed() and self.framework.is_installed()

    def get_include_dirs(self, board_config: Any) -> List[Path]:
        """Get include directories for Teensy builds.

        Args:
            board_config: Board configuration object

        Returns:
            List of include directory paths
        """
        includes = []

        # Core includes
        try:
            core_includes = self.framework.get_core_includes("teensy4")
            includes.extend(core_includes)
        except FrameworkErrorTeensy:
            pass

        return includes

    def get_core_sources(self) -> List[Path]:
        """Get core source files for Teensy builds.

        Returns:
            List of core source file paths
        """
        try:
            return self.framework.get_core_sources("teensy4")
        except FrameworkErrorTeensy:
            return []

    def get_toolchain_binaries(self) -> Dict[str, Path]:
        """Get paths to toolchain binaries.

        Returns:
            Dictionary mapping tool names to paths

        Raises:
            PlatformErrorTeensy: If toolchain binaries are not found
        """
        tools = self.toolchain.get_all_tool_paths()

        # Verify all required tools exist
        required_tools = ["gcc", "g++", "ar", "objcopy", "size"]
        for tool_name in required_tools:
            if tool_name not in tools or tools[tool_name] is None:
                raise PlatformErrorTeensy(f"Required tool not found: {tool_name}")

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
            board_id: Board identifier (e.g., "teensy41")

        Returns:
            Dictionary containing board configuration

        Raises:
            PlatformErrorTeensy: If board is not supported
        """
        from .. import platform_configs

        config = platform_configs.load_board_config(board_id)
        if config is None:
            available = [c for c in platform_configs.list_available_configs() if c.startswith("teensy")]
            raise PlatformErrorTeensy(f"Unsupported board: {board_id}. Available: {', '.join(available)}")

        # Transform to expected format for ConfigurableCompiler/Linker
        # All configuration is now data-driven from the JSON files
        return {
            "build": {
                "mcu": config.get("mcu", ""),
                "f_cpu": config.get("f_cpu", "600000000L"),
                "core": config.get("core", "teensy4"),
                "variant": config.get("variant", board_id),
                "board": config.get("board", board_id.upper()),
            },
            "name": config.get("name", board_id),
            "upload": config.get("upload", {}),
        }

    def get_platform_info(self) -> Dict[str, Any]:
        """Get information about the installed platform.

        Returns:
            Dictionary with platform information
        """
        info = {
            "platform": "teensy",
            "mcu": self.board_mcu,
            "installed": self.is_installed(),
            "toolchain": self.toolchain.get_toolchain_info(),
            "framework": self.framework.get_framework_info(),
        }

        return info
