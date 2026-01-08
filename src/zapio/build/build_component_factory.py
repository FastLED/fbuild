"""
Build component factory for Zapio build system.

This module provides factory methods for creating build components
(Compiler, Linker) with appropriate configurations. It centralizes
the logic for setting up these components with correct parameters.
"""

from pathlib import Path
from typing import List, Optional

from ..config.board_config import BoardConfig
from ..config.mcu_specs import get_max_flash, get_max_ram
from ..packages.package import IToolchain
from .avr_compiler import CompilerAVR
from .linker import LinkerAVR


class BuildComponentFactory:
    """
    Factory for creating build components with proper configurations.

    This class centralizes the creation of build system components (Compiler, Linker)
    with appropriate settings derived from board configuration and toolchain.

    Example usage:
        factory = BuildComponentFactory()
        compiler = factory.create_compiler(
            toolchain=toolchain,
            board_config=board_config,
            core_path=core_path,
            lib_include_paths=[Path("lib1/include"), Path("lib2/include")]
        )
        linker = factory.create_linker(
            toolchain=toolchain,
            board_config=board_config
        )
    """

    @staticmethod
    def create_compiler(
        toolchain: IToolchain,
        board_config: BoardConfig,
        core_path: Path,
        lib_include_paths: Optional[List[Path]] = None
    ) -> CompilerAVR:
        """
        Create compiler instance with appropriate settings.

        Configures the compiler with:
        - Toolchain binaries (avr-gcc, avr-g++)
        - MCU and F_CPU from board configuration
        - Include paths (core + variant + libraries)
        - Defines (Arduino version, board-specific defines)

        Args:
            toolchain: Toolchain instance
            board_config: Board configuration
            core_path: Arduino core path
            lib_include_paths: Optional library include paths

        Returns:
            Configured Compiler instance
        """
        # Get toolchain binaries
        tools = toolchain.get_all_tools()

        # Get include paths from board config
        include_paths = board_config.get_include_paths(core_path)

        # Add library include paths
        if lib_include_paths:
            include_paths = list(include_paths) + list(lib_include_paths)

        # Get defines from board config
        defines = board_config.get_defines()

        # Create and return compiler
        return CompilerAVR(
            avr_gcc=tools['avr-gcc'],
            avr_gpp=tools['avr-g++'],
            mcu=board_config.mcu,
            f_cpu=board_config.f_cpu,
            includes=include_paths,
            defines=defines
        )

    @staticmethod
    def create_linker(
        toolchain: IToolchain,
        board_config: BoardConfig
    ) -> LinkerAVR:
        """
        Create linker instance with appropriate settings.

        Configures the linker with:
        - Toolchain binaries (avr-gcc, avr-ar, avr-objcopy, avr-size)
        - MCU from board configuration
        - Flash and RAM limits from MCU specifications

        Args:
            toolchain: Toolchain instance
            board_config: Board configuration

        Returns:
            Configured Linker instance
        """
        # Get toolchain binaries
        tools = toolchain.get_all_tools()

        # Get MCU specifications from centralized module
        max_flash = get_max_flash(board_config.mcu)
        max_ram = get_max_ram(board_config.mcu)

        # Create and return linker
        return LinkerAVR(
            avr_gcc=tools['avr-gcc'],
            avr_ar=tools['avr-ar'],
            avr_objcopy=tools['avr-objcopy'],
            avr_size=tools['avr-size'],
            mcu=board_config.mcu,
            max_flash=max_flash,
            max_ram=max_ram
        )
