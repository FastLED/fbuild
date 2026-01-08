"""Abstract base classes for compilation components.

This module defines the interface for platform-specific compilers and linkers
to ensure consistent behavior across different platforms (AVR, ESP32, etc.).
"""

from abc import ABC, abstractmethod
from pathlib import Path
from typing import List, Dict, Optional
from dataclasses import dataclass


@dataclass
class CompileResult:
    """Result of a compilation operation."""
    success: bool
    object_file: Optional[Path]
    stdout: str
    stderr: str
    returncode: int


class CompilerError(Exception):
    """Base exception for compilation errors."""
    pass


class ICompiler(ABC):
    """Interface for source code compilers.

    This interface defines the common contract for all compiler implementations:
    - AVR Compiler (avr-gcc/avr-g++)
    - ESP32 Compiler (riscv32-esp-elf-gcc, xtensa-esp32-elf-gcc)
    - Configurable Compiler (platform-agnostic)
    """

    @abstractmethod
    def compile_source(
        self,
        source_path: Path,
        output_path: Optional[Path] = None
    ) -> Path:
        """Compile a single source file to object file.

        Args:
            source_path: Path to .c or .cpp source file
            output_path: Optional path for output .o file

        Returns:
            Path to generated .o file

        Raises:
            CompilerError: If compilation fails
        """
        pass

    @abstractmethod
    def get_include_paths(self) -> List[Path]:
        """Get all include paths needed for compilation.

        Returns:
            List of include directory paths
        """
        pass

    @abstractmethod
    def get_compile_flags(self) -> Dict[str, List[str]]:
        """Get compilation flags.

        Returns:
            Dictionary with 'cflags', 'cxxflags', and 'common' keys
        """
        pass

    @abstractmethod
    def needs_rebuild(self, source: Path, object_file: Path) -> bool:
        """Check if source file needs to be recompiled.

        Args:
            source: Source file path
            object_file: Object file path

        Returns:
            True if source is newer than object file or object doesn't exist
        """
        pass

    @abstractmethod
    def compile(
        self,
        source: Path,
        output: Path,
        extra_flags: Optional[List[str]] = None
    ) -> CompileResult:
        """Compile source file (auto-detects C vs C++).

        Args:
            source: Path to source file
            output: Path to output .o object file
            extra_flags: Additional compiler flags

        Returns:
            CompileResult with compilation status

        Raises:
            CompilerError: If compilation fails
        """
        pass


class ILinker(ABC):
    """Interface for linkers.

    This interface defines the common contract for all linker implementations:
    - AVR Linker (avr-gcc linker)
    - ESP32 Linker (riscv32/xtensa linker)
    - Configurable Linker (platform-agnostic)
    """

    @abstractmethod
    def link(
        self,
        sketch_objects: List[Path],
        core_archive: Path,
        output_elf: Optional[Path] = None,
        library_archives: Optional[List[Path]] = None
    ) -> Path:
        """Link object files into firmware ELF.

        Args:
            sketch_objects: List of sketch object files
            core_archive: Core archive file (core.a)
            output_elf: Optional path for output .elf file
            library_archives: Optional list of library archives

        Returns:
            Path to generated .elf file

        Raises:
            LinkerError: If linking fails
        """
        pass

    @abstractmethod
    def generate_bin(self, elf_path: Path) -> Path:
        """Generate binary from ELF file.

        For AVR: Generates .hex (Intel HEX format)
        For ESP32: Generates .bin (raw binary)

        Args:
            elf_path: Path to firmware.elf file

        Returns:
            Path to generated binary file

        Raises:
            LinkerError: If binary generation fails
        """
        pass


class LinkerError(Exception):
    """Base exception for linking errors."""
    pass
