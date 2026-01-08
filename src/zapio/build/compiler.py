"""
AVR compiler wrapper for building Arduino sketches.

This module provides a wrapper around avr-gcc and avr-g++ for compiling
C and C++ source files to object files.
"""

import subprocess
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


class Compiler:
    """
    Wrapper for AVR-GCC compiler.

    Compiles C and C++ source files to object files using avr-gcc and avr-g++
    with appropriate flags for Arduino builds.
    """

    def __init__(
        self,
        avr_gcc: Path,
        avr_gpp: Path,
        mcu: str,
        f_cpu: str,
        includes: List[Path],
        defines: Dict[str, str]
    ):
        """
        Initialize compiler.

        Args:
            avr_gcc: Path to avr-gcc executable
            avr_gpp: Path to avr-g++ executable
            mcu: MCU type (e.g., atmega328p)
            f_cpu: CPU frequency (e.g., 16000000L)
            includes: List of include directories
            defines: Dictionary of preprocessor defines
        """
        self.avr_gcc = Path(avr_gcc)
        self.avr_gpp = Path(avr_gpp)
        self.mcu = mcu
        self.f_cpu = f_cpu
        self.includes = [Path(p) for p in includes]
        self.defines = defines

        # Verify tools exist
        if not self.avr_gcc.exists():
            raise CompilerError(f"avr-gcc not found: {self.avr_gcc}")
        if not self.avr_gpp.exists():
            raise CompilerError(f"avr-g++ not found: {self.avr_gpp}")

    def compile_c(
        self,
        source: Path,
        output: Path,
        extra_flags: Optional[List[str]] = None
    ) -> CompileResult:
        """
        Compile C source file.

        Args:
            source: Path to .c source file
            output: Path to output .o object file
            extra_flags: Additional compiler flags

        Returns:
            CompileResult with compilation status
        """
        cmd = self._build_c_command(source, output, extra_flags or [])
        return self._execute_compiler(cmd, output)

    def compile_cpp(
        self,
        source: Path,
        output: Path,
        extra_flags: Optional[List[str]] = None
    ) -> CompileResult:
        """
        Compile C++ source file.

        Args:
            source: Path to .cpp source file
            output: Path to output .o object file
            extra_flags: Additional compiler flags

        Returns:
            CompileResult with compilation status
        """
        cmd = self._build_cpp_command(source, output, extra_flags or [])
        return self._execute_compiler(cmd, output)

    def compile(
        self,
        source: Path,
        output: Path,
        extra_flags: Optional[List[str]] = None
    ) -> CompileResult:
        """
        Compile source file (auto-detects C vs C++).

        Args:
            source: Path to source file
            output: Path to output .o object file
            extra_flags: Additional compiler flags

        Returns:
            CompileResult with compilation status
        """
        source = Path(source)

        if source.suffix == '.c':
            return self.compile_c(source, output, extra_flags)
        elif source.suffix in ['.cpp', '.cxx', '.cc']:
            return self.compile_cpp(source, output, extra_flags)
        else:
            raise CompilerError(f"Unknown source file type: {source.suffix}")

    def compile_sources(
        self,
        sources: List[Path],
        output_dir: Path,
        extra_flags: Optional[List[str]] = None
    ) -> List[Path]:
        """
        Compile multiple source files.

        Args:
            sources: List of source files
            output_dir: Output directory for object files
            extra_flags: Additional compiler flags

        Returns:
            List of compiled object file paths

        Raises:
            CompilerError: If any compilation fails
        """
        output_dir = Path(output_dir)
        output_dir.mkdir(parents=True, exist_ok=True)

        object_files = []

        for source in sources:
            source = Path(source)
            # Generate object file name
            obj_name = source.stem + '.o'
            obj_path = output_dir / obj_name

            # Compile
            result = self.compile(source, obj_path, extra_flags)

            if not result.success:
                raise CompilerError(
                    f"Failed to compile {source}:\n{result.stderr}"
                )

            object_files.append(obj_path)

        return object_files

    def needs_rebuild(self, source: Path, object_file: Path) -> bool:
        """
        Check if source file needs to be recompiled.

        Args:
            source: Source file path
            object_file: Object file path

        Returns:
            True if source is newer than object file
        """
        if not object_file.exists():
            return True

        source_mtime = source.stat().st_mtime
        obj_mtime = object_file.stat().st_mtime

        return source_mtime > obj_mtime

    def _build_c_command(
        self,
        source: Path,
        output: Path,
        extra_flags: List[str]
    ) -> List[str]:
        """Build avr-gcc command for C compilation."""
        cmd = [
            str(self.avr_gcc),
            '-c',              # Compile only, don't link
            '-g',              # Include debug symbols
            '-Os',             # Optimize for size
            '-w',              # Suppress warnings (matches Arduino)
            '-std=gnu11',      # C11 with GNU extensions
            '-ffunction-sections',  # Function sections for linker GC
            '-fdata-sections',      # Data sections for linker GC
            '-flto',           # Link-time optimization
            '-fno-fat-lto-objects',  # LTO bytecode only
            f'-mmcu={self.mcu}',    # Target MCU
        ]

        # Add defines
        for key, value in self.defines.items():
            if value:
                cmd.append(f'-D{key}={value}')
            else:
                cmd.append(f'-D{key}')

        # Add F_CPU explicitly
        if 'F_CPU' not in self.defines:
            cmd.append(f'-DF_CPU={self.f_cpu}')

        # Add include paths
        for include in self.includes:
            cmd.append(f'-I{include}')

        # Add extra flags
        cmd.extend(extra_flags)

        # Add source and output
        cmd.extend([str(source), '-o', str(output)])

        return cmd

    def _build_cpp_command(
        self,
        source: Path,
        output: Path,
        extra_flags: List[str]
    ) -> List[str]:
        """Build avr-g++ command for C++ compilation."""
        cmd = [
            str(self.avr_gpp),
            '-c',              # Compile only, don't link
            '-g',              # Include debug symbols
            '-Os',             # Optimize for size
            '-w',              # Suppress warnings (matches Arduino)
            '-std=gnu++11',    # C++11 with GNU extensions
            '-fpermissive',    # Allow some non-standard code
            '-fno-exceptions',  # Disable exceptions (no room on AVR)
            '-ffunction-sections',      # Function sections
            '-fdata-sections',          # Data sections
            '-fno-threadsafe-statics',  # No thread safety needed
            '-flto',           # Link-time optimization
            '-fno-fat-lto-objects',  # LTO bytecode only
            f'-mmcu={self.mcu}',        # Target MCU
        ]

        # Add defines
        for key, value in self.defines.items():
            if value:
                cmd.append(f'-D{key}={value}')
            else:
                cmd.append(f'-D{key}')

        # Add F_CPU explicitly
        if 'F_CPU' not in self.defines:
            cmd.append(f'-DF_CPU={self.f_cpu}')

        # Add include paths
        for include in self.includes:
            cmd.append(f'-I{include}')

        # Add extra flags
        cmd.extend(extra_flags)

        # Add source and output
        cmd.extend([str(source), '-o', str(output)])

        return cmd

    def _execute_compiler(
        self,
        cmd: List[str],
        output: Path
    ) -> CompileResult:
        """Execute compiler command."""
        try:
            result = subprocess.run(
                cmd,
                capture_output=True,
                text=True,
                check=False
            )

            success = result.returncode == 0
            obj_file = output if success and output.exists() else None

            return CompileResult(
                success=success,
                object_file=obj_file,
                stdout=result.stdout,
                stderr=result.stderr,
                returncode=result.returncode
            )

        except KeyboardInterrupt as ke:
            from zapio.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            return CompileResult(
                success=False,
                object_file=None,
                stdout='',
                stderr=str(e),
                returncode=-1
            )


class CompilerError(Exception):
    """Raised when compilation fails."""
    pass
