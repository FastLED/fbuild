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

    def get_compiler_flags(self, board_config: Any, mcu: str = "") -> List[str]:
        """Get compiler flags for STM32 builds.

        Args:
            board_config: Board configuration object
            mcu: MCU type (e.g., "stm32f446ret6")

        Returns:
            List of compiler flags
        """
        if not mcu:
            mcu = self.board_mcu

        cpu_type = self._get_cpu_type(mcu)
        mcu_family = self._get_mcu_family(mcu)

        # Base flags common to all STM32
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
            "-DARDUINO_ARCH_STM32",
            "-DSTM32_CORE_VERSION_MAJOR=2",
            "-DSTM32_CORE_VERSION_MINOR=12",
            f"-D{mcu_family.replace('xx', '')}",
        ]

        # CPU-specific flags
        if cpu_type == "cortex-m0plus":
            flags.extend(
                [
                    "-mcpu=cortex-m0plus",
                    "-mthumb",
                    "-march=armv6-m",
                ]
            )
        elif cpu_type == "cortex-m3":
            flags.extend(
                [
                    "-mcpu=cortex-m3",
                    "-mthumb",
                    "-march=armv7-m",
                ]
            )
        elif cpu_type == "cortex-m4":
            flags.extend(
                [
                    "-mcpu=cortex-m4",
                    "-mthumb",
                    "-march=armv7e-m",
                    "-mfloat-abi=hard",
                    "-mfpu=fpv4-sp-d16",
                ]
            )
        elif cpu_type == "cortex-m7":
            flags.extend(
                [
                    "-mcpu=cortex-m7",
                    "-mthumb",
                    "-march=armv7e-m",
                    "-mfloat-abi=hard",
                    "-mfpu=fpv5-sp-d16",
                ]
            )
        elif cpu_type == "cortex-m33":
            flags.extend(
                [
                    "-mcpu=cortex-m33",
                    "-mthumb",
                    "-march=armv8-m.main+dsp",
                    "-mfloat-abi=hard",
                    "-mfpu=fpv5-sp-d16",
                ]
            )

        # Add CPU frequency
        if hasattr(board_config, "f_cpu"):
            flags.append(f"-DF_CPU={board_config.f_cpu}")
        else:
            # Default frequencies by family
            default_freqs = {
                "STM32F0xx": "48000000L",
                "STM32F1xx": "72000000L",
                "STM32F2xx": "120000000L",
                "STM32F3xx": "72000000L",
                "STM32F4xx": "180000000L",
                "STM32F7xx": "216000000L",
                "STM32G0xx": "64000000L",
                "STM32G4xx": "170000000L",
                "STM32H7xx": "480000000L",
                "STM32L0xx": "32000000L",
                "STM32L1xx": "32000000L",
                "STM32L4xx": "80000000L",
                "STM32L5xx": "110000000L",
            }
            f_cpu = default_freqs.get(mcu_family, "72000000L")
            flags.append(f"-DF_CPU={f_cpu}")

        return flags

    def get_compiler_flags_cpp(self, board_config: Any, mcu: str = "") -> List[str]:
        """Get C++ compiler flags for STM32 builds.

        Args:
            board_config: Board configuration object
            mcu: MCU type (e.g., "stm32f446ret6")

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

    def get_linker_flags(self, board_config: Any, mcu: str = "") -> List[str]:
        """Get linker flags for STM32 builds.

        Args:
            board_config: Board configuration object
            mcu: MCU type (e.g., "stm32f446ret6")

        Returns:
            List of linker flags
        """
        if not mcu:
            mcu = self.board_mcu

        cpu_type = self._get_cpu_type(mcu)

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
            # Entry point
            "-Wl,--entry=Reset_Handler",
            # No default libs - we provide our own
            "--specs=nano.specs",
            "--specs=nosys.specs",
        ]

        # CPU-specific flags
        if cpu_type == "cortex-m0plus":
            flags.extend(
                [
                    "-mcpu=cortex-m0plus",
                    "-mthumb",
                ]
            )
        elif cpu_type == "cortex-m3":
            flags.extend(
                [
                    "-mcpu=cortex-m3",
                    "-mthumb",
                ]
            )
        elif cpu_type == "cortex-m4":
            flags.extend(
                [
                    "-mcpu=cortex-m4",
                    "-mthumb",
                    "-mfloat-abi=hard",
                    "-mfpu=fpv4-sp-d16",
                ]
            )
        elif cpu_type == "cortex-m7":
            flags.extend(
                [
                    "-mcpu=cortex-m7",
                    "-mthumb",
                    "-mfloat-abi=hard",
                    "-mfpu=fpv5-sp-d16",
                ]
            )
        elif cpu_type == "cortex-m33":
            flags.extend(
                [
                    "-mcpu=cortex-m33",
                    "-mthumb",
                    "-mfloat-abi=hard",
                    "-mfpu=fpv5-sp-d16",
                ]
            )

        return flags

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
        # Map board IDs to their configurations
        # Note: "core" must be "arduino" as that's the directory name in STM32duino framework
        board_configs = {
            "nucleo_f446re": {
                "build": {
                    "mcu": "stm32f446ret6",
                    "f_cpu": "180000000L",
                    "core": "arduino",
                    "cpu": "cortex-m4",
                    "variant": "STM32F4xx/F446R(C-E)T",
                    "extra_flags": "-DSTM32F4 -DSTM32F446xx",
                    "product_line": "STM32F446xx",
                },
                "name": "ST Nucleo F446RE",
                "upload": {
                    "maximum_size": 524288,  # 512KB flash
                    "maximum_ram_size": 131072,  # 128KB RAM
                },
            },
            "nucleo_f411re": {
                "build": {
                    "mcu": "stm32f411ret6",
                    "f_cpu": "100000000L",
                    "core": "arduino",
                    "cpu": "cortex-m4",
                    "variant": "STM32F4xx/F411R(C-E)T",
                    "extra_flags": "-DSTM32F4 -DSTM32F411xE",
                    "product_line": "STM32F411xE",
                },
                "name": "ST Nucleo F411RE",
                "upload": {
                    "maximum_size": 524288,  # 512KB flash
                    "maximum_ram_size": 131072,  # 128KB RAM
                },
            },
            "bluepill_f103c8": {
                "build": {
                    "mcu": "stm32f103c8t6",
                    "f_cpu": "72000000L",
                    "core": "arduino",
                    "cpu": "cortex-m3",
                    "variant": "STM32F1xx/F103C8T_F103CB(T-U)",
                    "extra_flags": "-DSTM32F1 -DSTM32F103xB",
                    "product_line": "STM32F103xB",
                },
                "name": "BluePill F103C8",
                "upload": {
                    "maximum_size": 65536,  # 64KB flash
                    "maximum_ram_size": 20480,  # 20KB RAM
                },
            },
            "blackpill_f411ce": {
                "build": {
                    "mcu": "stm32f411ceu6",
                    "f_cpu": "100000000L",
                    "core": "arduino",
                    "cpu": "cortex-m4",
                    "variant": "STM32F4xx/F411C(C-E)(U-Y)",
                    "extra_flags": "-DSTM32F4 -DSTM32F411xE",
                    "product_line": "STM32F411xE",
                },
                "name": "BlackPill F411CE",
                "upload": {
                    "maximum_size": 524288,  # 512KB flash
                    "maximum_ram_size": 131072,  # 128KB RAM
                },
            },
            "nucleo_l476rg": {
                "build": {
                    "mcu": "stm32l476rgt6",
                    "f_cpu": "80000000L",
                    "core": "arduino",
                    "cpu": "cortex-m4",
                    "variant": "STM32L4xx/L476R(C-E-G)T",
                    "extra_flags": "-DSTM32L4 -DSTM32L476xx",
                    "product_line": "STM32L476xx",
                },
                "name": "ST Nucleo L476RG",
                "upload": {
                    "maximum_size": 1048576,  # 1MB flash
                    "maximum_ram_size": 131072,  # 128KB RAM
                },
            },
            "nucleo_f103rb": {
                "build": {
                    "mcu": "stm32f103rbt6",
                    "f_cpu": "72000000L",
                    "core": "arduino",
                    "cpu": "cortex-m3",
                    "variant": "STM32F1xx/F103R(8-B)T",
                    "extra_flags": "-DSTM32F1 -DSTM32F103xB",
                    "product_line": "STM32F103xB",
                },
                "name": "ST Nucleo F103RB",
                "upload": {
                    "maximum_size": 131072,  # 128KB flash
                    "maximum_ram_size": 20480,  # 20KB RAM
                },
            },
        }

        if board_id not in board_configs:
            raise PlatformErrorSTM32(f"Unsupported board: {board_id}. " + f"Supported boards: {', '.join(board_configs.keys())}")

        return board_configs[board_id]

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
