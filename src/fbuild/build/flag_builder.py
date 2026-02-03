"""Compilation Flag Builder.

This module handles parsing and building compilation flags from platform
configuration files.

Design:
    - Parses flag strings with proper handling of quoted values
    - Builds flags from configuration dictionaries
    - Adds platform-specific defines (Arduino, ESP32, etc.)
    - Merges user build flags from platformio.ini
    - Applies build profile flags from BuildContext
"""

import shlex
from typing import TYPE_CHECKING, List, Dict

from .build_profiles import merge_compile_flags

if TYPE_CHECKING:
    from .build_context import BuildContext


class FlagBuilderError(Exception):
    """Raised when flag building operations fail."""
    pass


class FlagBuilder:
    """Builds compilation flags from configuration.

    This class handles:
    - Parsing flag strings with quoted values
    - Building flags from platform config
    - Adding platform-specific defines
    - Merging user build flags
    - Applying build profile flags from context
    """

    def __init__(self, context: "BuildContext"):
        """Initialize flag builder.

        Args:
            context: Build context containing all configuration
        """
        # Extract everything from context (no redundant parameters)
        self.config = context.platform_config
        self.board_config = context.board_config
        self.board_id = context.board_id
        self.variant = context.variant
        self.context = context
        self.user_build_flags = context.user_build_flags

        # Profile flags are pre-resolved in context
        self._profile_flags = context.profile_flags

    @staticmethod
    def parse_flag_string(flag_string: str) -> List[str]:
        """Parse a flag string that may contain quoted values.

        Args:
            flag_string: String containing compiler flags

        Returns:
            List of individual flags with quotes preserved

        Example:
            >>> FlagBuilder.parse_flag_string('-DFOO="bar baz" -DTEST')
            ['-DFOO="bar baz"', '-DTEST']
        """
        try:
            return shlex.split(flag_string)
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception:
            return flag_string.split()

    def build_flags(self) -> Dict[str, List[str]]:
        """Build compilation flags from configuration.

        Returns:
            Dictionary with 'cflags', 'cxxflags', and 'common' keys
        """
        flags = {
            'common': [],  # Common flags for both C and C++
            'cflags': [],  # C-specific flags
            'cxxflags': []  # C++-specific flags
        }

        # Get flags from config
        config_flags = self.config.get('compiler_flags', {})

        # Common flags (CCFLAGS in PlatformIO)
        if 'common' in config_flags:
            flags['common'] = config_flags['common'].copy()

        # C-specific flags (CFLAGS in PlatformIO)
        if 'c' in config_flags:
            flags['cflags'] = config_flags['c'].copy()

        # C++-specific flags (CXXFLAGS in PlatformIO)
        if 'cxx' in config_flags:
            flags['cxxflags'] = config_flags['cxx'].copy()

        # Apply profile-based flags to common (replaces existing optimization and LTO flags)
        self._apply_profile_flags(flags)

        # Add defines from config (CPPDEFINES in PlatformIO)
        defines = self.config.get('defines', [])
        for define in defines:
            if isinstance(define, str):
                flags['common'].append(f'-D{define}')
            elif isinstance(define, list) and len(define) == 2:
                flags['common'].append(f'-D{define[0]}={define[1]}')

        # Add Arduino-specific defines
        self._add_arduino_defines(flags)

        # Add board-specific extra flags if present
        self._add_board_extra_flags(flags)

        # Add user build flags from platformio.ini
        self._add_user_flags(flags)

        return flags

    def _apply_profile_flags(self, flags: Dict[str, List[str]]) -> None:
        """Apply profile-based compilation flags declaratively.

        Uses merge_compile_flags() to filter out profile-controlled flags
        from platform config and add profile-specific flags.

        Args:
            flags: Flags dictionary to update
        """
        flags['common'] = merge_compile_flags(flags['common'], self._profile_flags)

    def _add_arduino_defines(self, flags: Dict[str, List[str]]) -> None:
        """Add Arduino-specific defines to flags.

        Args:
            flags: Flags dictionary to update
        """
        build_config = self.board_config.get("build", {})
        f_cpu = build_config.get("f_cpu", "160000000L")
        mcu = build_config.get("mcu", "")
        board = build_config.get("board", self.board_id.upper().replace("-", "_"))

        # Detect platform from MCU
        is_stm32 = mcu.lower().startswith("stm32")
        is_esp32 = mcu.lower().startswith("esp32")

        # Common Arduino defines
        flags['common'].extend([
            f'-DF_CPU={f_cpu}',
            '-DARDUINO=10812',  # Arduino version
            f'-DARDUINO_BOARD="{board}"',
            f'-DARDUINO_VARIANT="{self.variant}"',
        ])

        if is_stm32:
            # STM32-specific defines
            flags['common'].extend([
                f'-DARDUINO_{board}',
                '-DARDUINO_ARCH_STM32',
            ])
            # Add product line define (e.g., STM32F103xB, STM32F446xx)
            product_line = build_config.get("product_line", "")
            if product_line:
                flags['common'].append(f'-D{product_line}')
            # Add VARIANT_H pointing to correct variant header
            if self.variant:
                # Variant header path relative to variants directory
                flags['common'].append(f'-DVARIANT_H="{self.variant}/variant.h"')
        elif is_esp32:
            # ESP32-specific defines
            flags['common'].extend([
                # '-DESP32',  # REMOVED: Redundant with ESP32 framework headers (causes "ESP32 redefined" error)
                f'-DARDUINO_{board}',
                '-DARDUINO_ARCH_ESP32',
            ])
        else:
            # Generic Arduino defines
            flags['common'].append(f'-DARDUINO_{board}')

    def _add_board_extra_flags(self, flags: Dict[str, List[str]]) -> None:
        """Add board-specific extra flags.

        Args:
            flags: Flags dictionary to update
        """
        build_config = self.board_config.get("build", {})
        # ESP32 boards: extra_flags is under build.arduino.extra_flags
        # Other boards: extra_flags may be directly under build.extra_flags
        arduino_config = build_config.get("arduino", {})
        extra_flags = arduino_config.get("extra_flags", build_config.get("extra_flags", ""))

        if extra_flags:
            if isinstance(extra_flags, str):
                flag_list = extra_flags.split()
            else:
                flag_list = extra_flags

            for flag in flag_list:
                if flag.startswith('-D'):
                    flags['common'].append(flag)

    def _add_user_flags(self, flags: Dict[str, List[str]]) -> None:
        """Add user build flags from platformio.ini.

        These override/extend board defaults.

        Args:
            flags: Flags dictionary to update
        """
        for flag in self.user_build_flags:
            if flag.startswith('-D'):
                # Add defines to common flags
                flags['common'].append(flag)
            # Could extend to handle other flag types if needed

    def get_base_flags_for_library(self) -> List[str]:
        """Get base compiler flags for library compilation.

        Returns:
            List of compiler flags suitable for library compilation
        """
        flags = self.build_flags()
        base_flags = flags['common'].copy()
        base_flags.extend(flags['cxxflags'])
        return base_flags
