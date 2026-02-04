"""
Type-safe board configuration models.

This module provides dataclass-based configuration structures to replace
dict.get() patterns throughout the codebase, providing better type safety,
IDE support, and validation.
"""

from dataclasses import dataclass, field
from typing import Any, Dict, List


@dataclass(frozen=True)
class CompilerFlags:
    """Compiler flag configuration."""

    common: List[str] = field(default_factory=list)
    c: List[str] = field(default_factory=list)
    cxx: List[str] = field(default_factory=list)


@dataclass(frozen=True)
class BuildProfile:
    """Build profile configuration (release, quick, etc.)."""

    compile_flags: List[str] = field(default_factory=list)
    link_flags: List[str] = field(default_factory=list)


@dataclass(frozen=True)
class BoardConfigModel:
    """
    Type-safe board configuration model.

    All board configurations (teensy41.json, esp32c6.json, etc.) are parsed
    into this structure, providing type safety and validation instead of
    dict.get() patterns.

    Attributes:
        name: Human-readable board name
        description: Board description
        board: Board identifier (uppercase, e.g., "TEENSY41")
        mcu: MCU identifier (lowercase, e.g., "imxrt1062")
        architecture: CPU architecture (e.g., "arm", "riscv32")
        f_cpu: CPU frequency as string with suffix (e.g., "600000000L")
        core: Core directory name (e.g., "teensy4", "esp32")
        variant: Variant directory name (e.g., "teensy41")
        compiler_flags: Compiler flags by category
        linker_flags: Linker flags list
        linker_scripts: Linker script filenames
        linker_libs: Linker library flags (e.g., ["-lm", "-lgcc"])
        defines: Preprocessor defines (list of strings or [key, value] pairs)
        profiles: Build profiles (release, quick, etc.)
        upload: Upload configuration (optional)
        esptool: ESP32-specific tool configuration (optional)
    """

    name: str
    mcu: str
    architecture: str
    board: str

    # Optional fields with defaults
    description: str = ""
    f_cpu: str = "16000000L"
    core: str = ""
    variant: str = ""
    product_line: str = ""  # STM32-specific product line (e.g., "STM32F103xB")

    # Nested structures
    compiler_flags: CompilerFlags = field(default_factory=CompilerFlags)
    linker_flags: List[str] = field(default_factory=list)
    linker_scripts: List[str] = field(default_factory=list)
    linker_libs: List[str] = field(default_factory=list)
    defines: List[Any] = field(default_factory=list)  # List[str] or List[List[str]]
    profiles: Dict[str, BuildProfile] = field(default_factory=dict)

    # Platform-specific optional configs
    upload: Dict[str, Any] = field(default_factory=dict)
    esptool: Dict[str, Any] = field(default_factory=dict)

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "BoardConfigModel":
        """
        Parse board configuration from dictionary.

        Args:
            data: Raw configuration dictionary from JSON

        Returns:
            Type-safe BoardConfigModel instance

        Raises:
            ValueError: If required fields are missing or invalid
        """
        # Required fields
        try:
            name = data["name"]
            mcu = data["mcu"]
            architecture = data["architecture"]
        except KeyError as e:
            raise ValueError(f"Missing required field in board config: {e}")

        # Optional fields
        description = data.get("description", "")
        board = data.get("board", mcu.upper())
        f_cpu = data.get("f_cpu", "16000000L")
        core = data.get("core", "")
        variant = data.get("variant", "")
        product_line = data.get("product_line", "")

        # Parse compiler flags
        compiler_flags_data = data.get("compiler_flags", {})
        compiler_flags = CompilerFlags(
            common=compiler_flags_data.get("common", []),
            c=compiler_flags_data.get("c", []),
            cxx=compiler_flags_data.get("cxx", []),
        )

        # Parse build profiles
        profiles_data = data.get("profiles", {})
        profiles = {
            name: BuildProfile(
                compile_flags=profile.get("compile_flags", []),
                link_flags=profile.get("link_flags", []),
            )
            for name, profile in profiles_data.items()
        }

        return cls(
            name=name,
            description=description,
            board=board,
            mcu=mcu,
            architecture=architecture,
            f_cpu=f_cpu,
            core=core,
            variant=variant,
            product_line=product_line,
            compiler_flags=compiler_flags,
            linker_flags=data.get("linker_flags", []),
            linker_scripts=data.get("linker_scripts", []),
            linker_libs=data.get("linker_libs", []),
            defines=data.get("defines", []),
            profiles=profiles,
            upload=data.get("upload", {}),
            esptool=data.get("esptool", {}),
        )

    def to_dict(self) -> Dict[str, Any]:
        """
        Convert back to dictionary format.

        Returns:
            Dictionary representation compatible with JSON serialization
        """
        return {
            "name": self.name,
            "description": self.description,
            "board": self.board,
            "mcu": self.mcu,
            "architecture": self.architecture,
            "f_cpu": self.f_cpu,
            "core": self.core,
            "variant": self.variant,
            "compiler_flags": {
                "common": self.compiler_flags.common,
                "c": self.compiler_flags.c,
                "cxx": self.compiler_flags.cxx,
            },
            "linker_flags": self.linker_flags,
            "linker_scripts": self.linker_scripts,
            "linker_libs": self.linker_libs,
            "defines": self.defines,
            "profiles": {name: {"compile_flags": profile.compile_flags, "link_flags": profile.link_flags} for name, profile in self.profiles.items()},
            "upload": self.upload,
            "esptool": self.esptool,
        }
