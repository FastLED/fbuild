"""STM32 Platform Package Management.

This module coordinates STM32 platform components including toolchain and framework.
It provides a unified interface for managing STM32 Arduino builds.

Platform Components:
    - ARM GCC Toolchain (arm-none-eabi-gcc)
    - STM32duino Framework (Arduino core for STM32)

Supported Boards:
    - BluePill F103C8 (STM32F103C8T6, ARM Cortex-M3 @ 72MHz)
    - Nucleo F446RE (STM32F446RET6, ARM Cortex-M4 @ 180MHz)
    - Nucleo F411RE (STM32F411RET6, ARM Cortex-M4 @ 100MHz)
    - Nucleo L476RG (STM32L476RGT6, ARM Cortex-M4 @ 80MHz)
    - And many more STM32 boards
"""

from pathlib import Path
from typing import Any, Dict, List

from .cache import Cache
from .framework_stm32 import FrameworkErrorSTM32, FrameworkSTM32
from .package import IPackage, PackageError
from .toolchain_stm32 import ToolchainErrorSTM32, ToolchainSTM32


class PlatformErrorSTM32(PackageError):
    """Raised when STM32 platform operations fail."""

    pass


class PlatformSTM32(IPackage):
    """Manages STM32 platform components and configuration.

    This class coordinates the ARM GCC toolchain and STM32duino framework to provide
    a complete build environment for STM32 boards.
    """

    def __init__(self, cache: Cache, board_mcu: str, show_progress: bool = True):
        """Initialize STM32 platform manager.

        Args:
            cache: Cache manager instance
            board_mcu: MCU type (e.g., "stm32f446ret6", "stm32f103c8t6")
            show_progress: Whether to show download/extraction progress
        """
        self.cache = cache
        self.board_mcu = board_mcu
        self.show_progress = show_progress

        # Initialize toolchain and framework
        self.toolchain = ToolchainSTM32(cache, show_progress=show_progress)
        self.framework = FrameworkSTM32(cache, show_progress=show_progress)

    def ensure_package(self) -> Path:
        """Ensure platform components are downloaded and extracted.

        Returns:
            Path to the framework directory (main platform directory)

        Raises:
            PlatformErrorSTM32: If download or extraction fails
        """
        try:
            # Ensure toolchain is installed
            self.toolchain.ensure_toolchain()

            # Ensure framework is installed
            framework_path = self.framework.ensure_framework()

            return framework_path

        except (ToolchainErrorSTM32, FrameworkErrorSTM32) as e:
            raise PlatformErrorSTM32(f"Failed to install STM32 platform: {e}")
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise PlatformErrorSTM32(f"Unexpected error installing platform: {e}")

    def is_installed(self) -> bool:
        """Check if platform is already installed.

        Returns:
            True if both toolchain and framework are installed
        """
        return self.toolchain.is_installed() and self.framework.is_installed()

    def _get_mcu_family(self, mcu: str) -> str:
        """Extract MCU family from MCU name.

        Args:
            mcu: MCU name (e.g., "stm32f446ret6")

        Returns:
            MCU family (e.g., "STM32F4xx")
        """
        mcu_upper = mcu.upper()
        if mcu_upper.startswith("STM32F0"):
            return "STM32F0xx"
        elif mcu_upper.startswith("STM32F1"):
            return "STM32F1xx"
        elif mcu_upper.startswith("STM32F2"):
            return "STM32F2xx"
        elif mcu_upper.startswith("STM32F3"):
            return "STM32F3xx"
        elif mcu_upper.startswith("STM32F4"):
            return "STM32F4xx"
        elif mcu_upper.startswith("STM32F7"):
            return "STM32F7xx"
        elif mcu_upper.startswith("STM32G0"):
            return "STM32G0xx"
        elif mcu_upper.startswith("STM32G4"):
            return "STM32G4xx"
        elif mcu_upper.startswith("STM32H7"):
            return "STM32H7xx"
        elif mcu_upper.startswith("STM32L0"):
            return "STM32L0xx"
        elif mcu_upper.startswith("STM32L1"):
            return "STM32L1xx"
        elif mcu_upper.startswith("STM32L4"):
            return "STM32L4xx"
        elif mcu_upper.startswith("STM32L5"):
            return "STM32L5xx"
        elif mcu_upper.startswith("STM32U5"):
            return "STM32U5xx"
        elif mcu_upper.startswith("STM32WB"):
            return "STM32WBxx"
        elif mcu_upper.startswith("STM32WL"):
            return "STM32WLxx"
        else:
            return "STM32F4xx"  # Default fallback

    def _get_cpu_type(self, mcu: str) -> str:
        """Get CPU type from MCU name.

        Args:
            mcu: MCU name (e.g., "stm32f446ret6")

        Returns:
            CPU type (e.g., "cortex-m4")
        """
        mcu_upper = mcu.upper()
        if mcu_upper.startswith("STM32F0") or mcu_upper.startswith("STM32G0") or mcu_upper.startswith("STM32L0"):
            return "cortex-m0plus"
        elif mcu_upper.startswith("STM32F1") or mcu_upper.startswith("STM32F2") or mcu_upper.startswith("STM32L1"):
            return "cortex-m3"
        elif mcu_upper.startswith("STM32F3") or mcu_upper.startswith("STM32F4") or mcu_upper.startswith("STM32G4") or mcu_upper.startswith("STM32L4"):
            return "cortex-m4"
        elif mcu_upper.startswith("STM32F7") or mcu_upper.startswith("STM32H7"):
            return "cortex-m7"
        elif mcu_upper.startswith("STM32L5") or mcu_upper.startswith("STM32U5"):
            return "cortex-m33"
        else:
            return "cortex-m4"  # Default fallback

    def get_include_dirs(self, board_config: Any) -> List[Path]:
        """Get include directories for STM32 builds.

        Args:
            board_config: Board configuration object

        Returns:
            List of include directory paths
        """
        includes = []

        # Core includes
        try:
            core_includes = self.framework.get_core_includes("arduino")
            includes.extend(core_includes)
        except FrameworkErrorSTM32:
            pass

        # Variant includes (if board_config has variant info)
        if hasattr(board_config, "variant"):
            variant_dir = self.framework.get_variant_dir(board_config.variant)
            if variant_dir:
                includes.append(variant_dir)

        # System includes
        system_dir = self.framework.framework_path / "system"
        if system_dir.exists():
            includes.append(system_dir)

        return includes

    def get_core_sources(self) -> List[Path]:
        """Get core source files for STM32 builds.

        Returns:
            List of core source file paths
        """
        try:
            return self.framework.get_core_sources("arduino")
        except FrameworkErrorSTM32:
            return []

    def get_toolchain_binaries(self) -> Dict[str, Path]:
        """Get paths to toolchain binaries.

        Returns:
            Dictionary mapping tool names to paths

        Raises:
            PlatformErrorSTM32: If toolchain binaries are not found
        """
        tools = self.toolchain.get_all_tool_paths()

        # Verify all required tools exist
        required_tools = ["gcc", "g++", "ar", "objcopy", "size"]
        for tool_name in required_tools:
            if tool_name not in tools or tools[tool_name] is None:
                raise PlatformErrorSTM32(f"Required tool not found: {tool_name}")

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
            board_id: Board identifier (e.g., "nucleo_f446re", "bluepill_f103c8")

        Returns:
            Dictionary containing board configuration

        Raises:
            PlatformErrorSTM32: If board is not supported
        """
        from .. import platform_configs

        config = platform_configs.load_board_config(board_id)
        if config is None:
            available = [c for c in platform_configs.list_available_configs() if "stm32" in c.lower() or "nucleo" in c or "bluepill" in c or "blackpill" in c]
            raise PlatformErrorSTM32(f"Unsupported board: {board_id}. Available: {', '.join(available)}")

        # Transform to expected format for ConfigurableCompiler/Linker - type-safe access
        return {
            "build": {
                "mcu": config.mcu,
                "f_cpu": config.f_cpu,
                "core": "arduino",
                "cpu": config.architecture.replace("arm-", ""),
                "variant": config.variant,
                "extra_flags": " ".join(f"-D{d}" for d in config.defines if isinstance(d, str) and d.startswith("STM32")),
                "product_line": config.product_line,
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
            "platform": "ststm32",
            "mcu": self.board_mcu,
            "installed": self.is_installed(),
            "toolchain": self.toolchain.get_toolchain_info(),
            "framework": self.framework.get_framework_info(),
        }

        return info
