"""Library compilation utilities for Zapio.

This module handles compiling external libraries into static archives (.a files)
with Link-Time Optimization (LTO) support.
"""

import subprocess
from pathlib import Path
from typing import List, Tuple


class LibraryCompilationError(Exception):
    """Exception raised for library compilation errors."""

    pass


class LibraryCompiler:
    """Handles compilation of external libraries into static archives."""

    @staticmethod
    def needs_rebuild(
        archive_file: Path,
        info_file: Path,
        compiler_flags: List[str],
        get_info_func,
    ) -> Tuple[bool, str]:
        """Check if a library needs to be rebuilt.

        Args:
            archive_file: Path to the .a archive file
            info_file: Path to the info.json file
            compiler_flags: Current compiler flags
            get_info_func: Function to load library info from JSON

        Returns:
            Tuple of (needs_rebuild, reason)
        """
        if not archive_file.exists():
            return True, "Archive not found"

        if not info_file.exists():
            return True, "Info file missing"

        info = get_info_func()
        if info is None:
            return True, "Could not load info"

        # Check if compile commands changed
        current_compile_cmd = " ".join(compiler_flags)
        stored_compile_cmd = " ".join(info.compile_commands)

        if current_compile_cmd != stored_compile_cmd:
            return True, "Compiler flags changed"

        return False, ""

    @staticmethod
    def compile_library(
        library_name: str,
        lib_dir: Path,
        source_files: List[Path],
        include_dirs: List[Path],
        compiler_path: Path,
        mcu: str,
        f_cpu: str,
        defines: List[str],
        extra_flags: List[str],
        show_progress: bool = True,
    ) -> Tuple[Path, List[Path], List[str]]:
        """Compile a library into a static archive (.a file).

        This function compiles all source files in a library and creates a static
        archive. It uses Link-Time Optimization (LTO) with -fno-fat-lto-objects
        to generate only LTO bytecode, avoiding assembly errors with complex code.

        Args:
            library_name: Name of the library
            lib_dir: Root directory for the library
            source_files: List of source files to compile
            include_dirs: Include directories for compilation
            compiler_path: Path to avr-gcc/avr-g++
            mcu: MCU target (e.g., atmega328p)
            f_cpu: CPU frequency (e.g., 16000000L)
            defines: Preprocessor defines
            extra_flags: Additional compiler flags
            show_progress: Whether to show progress

        Returns:
            Tuple of (archive_path, object_files, compile_commands)

        Raises:
            LibraryCompilationError: If compilation fails
        """
        try:
            if show_progress:
                print(f"Compiling library: {library_name}")

            if not source_files:
                raise LibraryCompilationError(
                    f"No source files found in library '{library_name}'"
                )

            # Compile each source file
            object_files = []
            compile_commands = []
            gcc_path = compiler_path.parent / "avr-gcc"
            gxx_path = compiler_path.parent / "avr-g++"

            for source in source_files:
                # Determine compiler based on extension
                if source.suffix in [".cpp", ".cc", ".cxx"]:
                    compiler = gxx_path
                    std_flag = "-std=gnu++11"
                else:
                    compiler = gcc_path
                    std_flag = "-std=gnu11"

                # Output object file
                obj_file = lib_dir / f"{source.stem}.o"

                # Build compile command
                # Use -flto with -fno-fat-lto-objects to generate only LTO bytecode
                # This avoids assembly errors with complex code like FastLED
                # The trade-off is we must link with object files, not archives
                cmd = [
                    str(compiler),
                    "-c",
                    "-g",
                    "-Os",
                    std_flag,
                    "-ffunction-sections",
                    "-fdata-sections",
                    "-flto",
                    "-fno-fat-lto-objects",  # LTO bytecode only, no assembly
                    f"-mmcu={mcu}",
                ]

                # Add defines from list (format: "KEY=value" or "KEY")
                for define in defines:
                    cmd.append(f"-D{define}")

                # Add include paths
                for inc_path in include_dirs:
                    cmd.append(f"-I{inc_path}")

                # Add extra flags
                cmd.extend(extra_flags)

                # Add source and output
                cmd.extend(["-o", str(obj_file), str(source)])

                # Store command for rebuild detection
                compile_commands.append(" ".join(cmd))

                # Compile
                if show_progress:
                    print(f"  Compiling {source.name}...")

                result = subprocess.run(
                    cmd, capture_output=True, text=True, encoding="utf-8"
                )

                if result.returncode != 0:
                    raise LibraryCompilationError(
                        f"Failed to compile {source}:\n{result.stderr}"
                    )

                object_files.append(obj_file)

            # Create static archive using avr-ar
            ar_path = compiler_path.parent / "avr-ar"
            archive_file = lib_dir / f"lib{library_name}.a"

            if show_progress:
                print(f"  Creating archive: {archive_file.name}")

            # Remove old archive if exists
            if archive_file.exists():
                archive_file.unlink()

            # Create new archive
            cmd = [str(ar_path), "rcs", str(archive_file)] + [
                str(obj) for obj in object_files
            ]

            result = subprocess.run(
                cmd, capture_output=True, text=True, encoding="utf-8"
            )

            if result.returncode != 0:
                raise LibraryCompilationError(
                    f"Failed to create archive for {library_name}:\n{result.stderr}"
                )

            # Keep object files for LTO linking (don't delete them)
            # Object files are needed for proper LTO symbol resolution

            if show_progress:
                print(f"Library '{library_name}' compiled successfully")

            return archive_file, object_files, compile_commands

        except subprocess.CalledProcessError as e:
            raise LibraryCompilationError(
                f"Compilation failed for library '{library_name}': {e}"
            ) from e
        except KeyboardInterrupt as ke:
            from zapio.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise LibraryCompilationError(
                f"Failed to compile library '{library_name}': {e}"
            ) from e
