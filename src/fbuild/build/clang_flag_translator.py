"""Clang Flag Translator - GCC cross-compiler to clang flag translation.

Translates GCC cross-compiler flags to clang-compatible equivalents for use
with clang-based analysis tools (clangd, clang-tidy, IWYU).

Each platform (AVR, ESP32 Xtensa, ESP32 RISC-V, ARM, RP2040) has specific
GCC flags that either need translation or removal for clang compatibility.
"""

import re
from dataclasses import dataclass


@dataclass(frozen=True)
class FlagRule:
    """A single flag translation rule.

    Attributes:
        pattern: Regex pattern to match the GCC flag
        replacement: Replacement string (None to remove the flag entirely)
    """

    pattern: str
    replacement: str | None


class ClangFlagTranslator:
    """Translates GCC cross-compiler flags to clang-compatible equivalents.

    Usage:
        translated = ClangFlagTranslator.translate(
            flags=["xtensa-esp-elf-gcc", "-mlongcalls", "-c", "src.c"],
            architecture="xtensa",
            mcu="esp32",
        )
    """

    # Flags to remove unconditionally (all platforms)
    _COMMON_REMOVE: list[FlagRule] = [
        FlagRule(pattern=r"^-flto=auto$", replacement=None),
        FlagRule(pattern=r"^-flto$", replacement=None),
        FlagRule(pattern=r"^-fno-fat-lto-objects$", replacement=None),
        FlagRule(pattern=r"^-fuse-linker-plugin$", replacement=None),
        FlagRule(pattern=r"^-ffat-lto-objects$", replacement=None),
    ]

    # Architecture-specific translation rules
    _ARCH_RULES: dict[str, list[FlagRule]] = {
        "xtensa": [
            # Xtensa-specific flags not supported by clang
            FlagRule(pattern=r"^-mlongcalls$", replacement=None),
            FlagRule(pattern=r"^-mdisable-hardware-atomics$", replacement=None),
            FlagRule(pattern=r"^-mfix-esp32-psram-cache-issue$", replacement=None),
            FlagRule(pattern=r"^-mfix-esp32-psram-cache-strategy=.*$", replacement=None),
            FlagRule(pattern=r"^-fstrict-volatile-bitfields$", replacement=None),
            FlagRule(pattern=r"^-mtext-section-literals$", replacement=None),
            FlagRule(pattern=r"^-fno-tree-switch-conversion$", replacement=None),
        ],
        "riscv32": [
            # RISC-V flags handled by target triple
            FlagRule(pattern=r"^-mabi=ilp32$", replacement=None),
            FlagRule(pattern=r"^-mno-fdiv$", replacement=None),
        ],
        "avr": [
            # AVR flags — -mmcu is kept, target triple added separately
        ],
        "arm": [
            # ARM flags — mostly compatible, just need target triple
            FlagRule(pattern=r"^-mthumb-interwork$", replacement=None),
        ],
    }

    # Map architecture to clang target triple prefix
    _TARGET_TRIPLES: dict[str, str] = {
        "xtensa": "xtensa-esp-elf",
        "riscv32": "riscv32-esp-elf",
        "avr": "avr",
        "arm": "arm-none-eabi",
    }

    @classmethod
    def translate(cls, flags: list[str], architecture: str, mcu: str) -> list[str]:
        """Translate a list of GCC flags to clang-compatible equivalents.

        The first element is treated as the compiler path and replaced with 'clang'
        (or 'clang++' if it ends with 'g++').

        Args:
            flags: Full compiler command as list (first element = compiler path)
            architecture: CPU architecture from BoardConfigModel (e.g., 'xtensa', 'arm', 'avr')
            mcu: MCU identifier (e.g., 'esp32c6', 'atmega328p', 'stm32f407vg')

        Returns:
            New list with clang-compatible flags
        """
        if not flags:
            return []

        result: list[str] = []

        # Replace compiler path with clang/clang++
        compiler = flags[0].lower().replace("\\", "/")
        if compiler.endswith("g++") or compiler.endswith("g++.exe"):
            result.append("clang++")
        else:
            result.append("clang")

        # Add target triple
        target = cls.get_target_triple(architecture, mcu)
        if target:
            result.append(f"--target={target}")

        # Collect all removal/translation rules for this architecture
        rules = list(cls._COMMON_REMOVE)
        arch_rules = cls._ARCH_RULES.get(architecture, [])
        rules.extend(arch_rules)

        # Process remaining flags
        for flag in flags[1:]:
            matched = False
            for rule in rules:
                if re.match(rule.pattern, flag):
                    if rule.replacement is not None:
                        result.append(rule.replacement)
                    # else: flag is removed
                    matched = True
                    break

            if not matched:
                result.append(flag)

        return result

    @classmethod
    def get_target_triple(cls, architecture: str, mcu: str) -> str | None:
        """Get the clang target triple for a given architecture and MCU.

        Args:
            architecture: CPU architecture (e.g., 'xtensa', 'arm', 'avr', 'riscv32')
            mcu: MCU identifier

        Returns:
            Target triple string, or None if architecture is unknown
        """
        return cls._TARGET_TRIPLES.get(architecture)
