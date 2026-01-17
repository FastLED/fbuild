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

    def get_compiler_flags(self, board_config: Any, mcu: str = "rp2040") -> List[str]:
        """Get compiler flags for RP2040/RP2350 builds.

        Args:
            board_config: Board configuration object
            mcu: MCU type ("rp2040" or "rp2350")

        Returns:
            List of compiler flags
        """
        # Base flags common to both RP2040 and RP2350
        flags = [
            # Optimization and debug
            "-Os",
            "-g",
            "-ffunction-sections",
            "-fdata-sections",
            "-fno-exceptions",
            # Warnings
            "-Wall",
            "-Wno-unused-parameter",
            "-Wno-sign-compare",
            "-Wno-error=unused-function",
            "-Wno-error=unused-variable",
            "-Wno-error=deprecated-declarations",
            # Standards
            "-std=gnu17",  # For C files
            # Arduino defines
            "-DARDUINO_ARCH_RP2040",
            "-DPICO_BOARD",
            "-DPICO_BUILD",
            "-DPICO_NO_HARDWARE",
            "-DPICO_ON_DEVICE",
            "-DCFG_TUSB_MCU=OPT_MCU_RP2040",
            "-DLWIP_IPV4=1",
            "-DLWIP_IPV6=0",
        ]

        # MCU-specific flags
        if mcu.lower() == "rp2350":
            # RP2350: Cortex-M33 with FPU and DSP
            flags.extend(
                [
                    "-march=armv8-m.main+fp+dsp",
                    "-mcpu=cortex-m33",
                    "-mthumb",
                    "-mfloat-abi=hard",
                    "-mfpu=fpv5-sp-d16",
                    f"-DF_CPU={getattr(board_config, 'f_cpu', '150000000L')}",
                    "-DTARGET_RP2350=1",
                    "-DPICO_RP2350=1",
                    "-DPICO_RP2350A",
                    "-D__ARM_FEATURE_DSP=1",
                ]
            )
        else:
            # RP2040: Cortex-M0+
            flags.extend(
                [
                    "-march=armv6-m",
                    "-mcpu=cortex-m0plus",
                    "-mthumb",
                    f"-DF_CPU={getattr(board_config, 'f_cpu', '133000000L')}",
                    "-DTARGET_RP2040=1",
                    "-DPICO_RP2040=1",
                ]
            )

        return flags

    def get_compiler_flags_cpp(self, board_config: Any, mcu: str = "rp2040") -> List[str]:
        """Get C++ compiler flags for RP2040/RP2350 builds.

        Args:
            board_config: Board configuration object
            mcu: MCU type ("rp2040" or "rp2350")

        Returns:
            List of C++ compiler flags
        """
        # Start with base C flags
        flags = self.get_compiler_flags(board_config, mcu)

        # Replace C standard with C++ standard
        flags = [f for f in flags if not f.startswith("-std=gnu17")]
        flags.extend(
            [
                "-std=gnu++17",
                "-fno-exceptions",
                "-fno-rtti",
                "-fno-threadsafe-statics",
            ]
        )

        return flags

    def get_linker_flags(self, board_config: Any, mcu: str = "rp2040") -> List[str]:
        """Get linker flags for RP2040/RP2350 builds.

        Args:
            board_config: Board configuration object
            mcu: MCU type ("rp2040" or "rp2350")

        Returns:
            List of linker flags
        """
        flags = [
            # Optimization
            "-Os",
            # Linker options
            "-Wl,--cref",
            "-Wl,--check-sections",
            "-Wl,--gc-sections",
            "-Wl,--unresolved-symbols=report-all",
            "-Wl,--warn-common",
            "-Wl,--warn-section-align",
            # Memory wrappers
            "-Wl,--wrap=malloc",
            "-Wl,--wrap=calloc",
            "-Wl,--wrap=realloc",
            "-Wl,--wrap=free",
            # Printf/scanf float support
            "-u",
            "_printf_float",
            "-u",
            "_scanf_float",
        ]

        # MCU-specific flags
        if mcu.lower() == "rp2350":
            flags.extend(
                [
                    "-march=armv8-m.main+fp+dsp",
                    "-mcpu=cortex-m33",
                    "-mthumb",
                    "-mfloat-abi=hard",
                    "-mfpu=fpv5-sp-d16",
                ]
            )
        else:
            flags.extend(
                [
                    "-march=armv6-m",
                    "-mcpu=cortex-m0plus",
                    "-mthumb",
                ]
            )

        return flags

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
        # Map board IDs to their configurations
        board_configs = {
            "rpipico": {
                "build": {
                    "mcu": "rp2040",
                    "f_cpu": "133000000L",
                    "core": "rp2040",
                    "variant": "rpipico",
                    "board": "RASPBERRY_PI_PICO",
                },
                "name": "Raspberry Pi Pico",
                "upload": {
                    "maximum_size": 2097152,  # 2MB flash
                    "maximum_ram_size": 270336,  # 264KB RAM
                },
            },
            "rpipicow": {
                "build": {
                    "mcu": "rp2040",
                    "f_cpu": "133000000L",
                    "core": "rp2040",
                    "variant": "rpipicow",
                    "board": "RASPBERRY_PI_PICO_W",
                },
                "name": "Raspberry Pi Pico W",
                "upload": {
                    "maximum_size": 2097152,  # 2MB flash
                    "maximum_ram_size": 270336,  # 264KB RAM
                },
            },
            "rpipico2": {
                "build": {
                    "mcu": "rp2350",
                    "f_cpu": "150000000L",
                    "core": "rp2040",
                    "variant": "rpipico2",
                    "board": "RASPBERRY_PI_PICO_2",
                },
                "name": "Raspberry Pi Pico 2",
                "upload": {
                    "maximum_size": 4194304,  # 4MB flash
                    "maximum_ram_size": 532480,  # 520KB RAM
                },
            },
            "rpipico2w": {
                "build": {
                    "mcu": "rp2350",
                    "f_cpu": "150000000L",
                    "core": "rp2040",
                    "variant": "rpipico2w",
                    "board": "RASPBERRY_PI_PICO_2_W",
                },
                "name": "Raspberry Pi Pico 2 W",
                "upload": {
                    "maximum_size": 4194304,  # 4MB flash
                    "maximum_ram_size": 532480,  # 520KB RAM
                },
            },
        }

        if board_id not in board_configs:
            raise PlatformErrorRP2040(f"Unsupported board: {board_id}. " + f"Supported boards: {', '.join(board_configs.keys())}")

        return board_configs[board_id]

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
