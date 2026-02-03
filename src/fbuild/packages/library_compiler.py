"""Library compilation utilities for fbuild.

This module handles compiling external libraries into static archives (.a files)
with Link-Time Optimization (LTO) support.
"""

import subprocess
from pathlib import Path
from typing import TYPE_CHECKING, Callable, List, Optional, Tuple

from fbuild.build.build_profiles import BuildProfile, get_profile
from fbuild.output import ProgressCallback, log_detail
from fbuild.subprocess_utils import safe_run

if TYPE_CHECKING:
    from .library_manager import LibraryInfo


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
        get_info_func: "Callable[[], Optional[LibraryInfo]]",
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
        progress_callback: ProgressCallback | None = None,
        profile: BuildProfile = BuildProfile.RELEASE,
    ) -> Tuple[Path, List[Path], List[str]]:
        """Compile a library into a static archive (.a file).

        This function compiles all source files in a library and creates a static
        archive. The build profile determines optimization and other compiler flags.

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
            progress_callback: Optional progress callback for UI feedback
            profile: Build profile

        Returns:
            Tuple of (archive_path, object_files, compile_commands)

        Raises:
            LibraryCompilationError: If compilation fails
        """
        try:
            if show_progress:
                log_detail(f"Compiling library: {library_name}")

            if not source_files:
                raise LibraryCompilationError(f"No source files found in library '{library_name}'")

            # Compile each source file
            object_files = []
            compile_commands = []

            # Derive toolchain prefix from compiler path for platform-agnostic support
            # e.g., "avr-g++" -> "avr", "arm-none-eabi-g++" -> "arm-none-eabi"
            compiler_name = compiler_path.stem  # e.g., "avr-g++" (without extension)
            if compiler_name.endswith("-g++"):
                prefix = compiler_name[:-4]  # Remove "-g++"
            elif compiler_name.endswith("-gcc"):
                prefix = compiler_name[:-4]  # Remove "-gcc"
            else:
                prefix = "avr"  # Fallback for AVR

            gcc_path = compiler_path.parent / f"{prefix}-gcc"
            gxx_path = compiler_path.parent / f"{prefix}-g++"

            # Determine if this is an AVR platform (uses -mmcu flag) or ARM (uses -mcpu from extra_flags)
            is_avr = prefix == "avr"

            # Find common base directory for relative path display
            # Try to use lib_dir as base, or find common parent of all sources
            try:
                base_dir = lib_dir
                # Verify all sources are under lib_dir
                for src in source_files:
                    src.relative_to(lib_dir)
            except ValueError:
                # If not all under lib_dir, find common parent
                if source_files:
                    base_dir = Path(*Path(source_files[0]).parts[:1])  # Start with root
                    for src in source_files:
                        try:
                            src.relative_to(base_dir)
                        except ValueError:
                            # Find common parent
                            parts1 = base_dir.parts
                            parts2 = src.parts
                            common = []
                            for p1, p2 in zip(parts1, parts2):
                                if p1 == p2:
                                    common.append(p1)
                                else:
                                    break
                            base_dir = Path(*common) if common else Path(".")
                else:
                    base_dir = lib_dir

            for source in source_files:
                # Determine compiler based on extension
                if source.suffix in [".cpp", ".cc", ".cxx"]:
                    compiler = gxx_path
                    std_flag = "-std=gnu++11"
                else:
                    compiler = gcc_path
                    std_flag = "-std=gnu11"

                # Output object file with unique name based on relative path
                # This prevents collisions when multiple source files have the same base name
                # (e.g., fl/_build.cpp and platforms/_build.cpp)
                try:
                    rel_path = source.relative_to(base_dir)
                    # Replace path separators with underscores to create unique filename
                    unique_name = str(rel_path).replace("/", "_").replace("\\", "_")
                    unique_name = unique_name.rsplit(".", 1)[0]  # Remove extension
                except ValueError:
                    unique_name = source.stem
                obj_file = lib_dir / f"{unique_name}.o"

                # Build compile command
                # Get profile flags for profile-aware compilation
                profile_flags = get_profile(profile)

                cmd = [
                    str(compiler),
                    "-c",
                    "-g",
                    std_flag,
                ]

                # Add all profile compile flags (optimization, section flags, LTO if enabled)
                cmd.extend(profile_flags.compile_flags)

                # Add MCU-specific flags:
                # - AVR uses -mmcu={mcu} (e.g., -mmcu=atmega328p)
                # - ARM uses -mcpu=... from extra_flags (e.g., -mcpu=cortex-m7)
                if is_avr:
                    cmd.append(f"-mmcu={mcu}")

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

                # Compute relative path for display (especially useful for unity builds)
                try:
                    rel_path_str = str(source.relative_to(base_dir))
                except ValueError:
                    # Fallback to filename if relative path fails
                    rel_path_str = source.name

                # Compile
                file_index = source_files.index(source) + 1  # 1-based indexing
                total_files = len(source_files)
                if progress_callback:
                    progress_callback.on_file_start(rel_path_str, file_index, total_files)
                elif show_progress:
                    log_detail(f"Compiling {rel_path_str}...")

                result = safe_run(cmd, capture_output=True, text=True, encoding="utf-8")

                if result.returncode != 0:
                    raise LibraryCompilationError(f"Failed to compile {source}:\n{result.stderr}")

                if progress_callback:
                    progress_callback.on_file_complete(rel_path_str, file_index, total_files)

                object_files.append(obj_file)

            # Create static archive using gcc-ar for LTO support
            # gcc-ar is an LTO-aware wrapper that creates proper symbol indices
            # for archives containing LTO bytecode objects (-flto -fno-fat-lto-objects)
            # Use the derived prefix for platform-agnostic support
            gcc_ar_path = compiler_path.parent / f"{prefix}-gcc-ar"
            ar_path = gcc_ar_path if gcc_ar_path.exists() else compiler_path.parent / f"{prefix}-ar"
            archive_file = lib_dir / f"lib{library_name}.a"

            if show_progress:
                log_detail(f"Creating archive: {archive_file.name}")

            # Remove old archive if exists
            if archive_file.exists():
                archive_file.unlink()

            # Create new archive
            cmd = [str(ar_path), "rcs", str(archive_file)] + [str(obj) for obj in object_files]

            result = safe_run(cmd, capture_output=True, text=True, encoding="utf-8")

            if result.returncode != 0:
                raise LibraryCompilationError(f"Failed to create archive for {library_name}:\n{result.stderr}")

            # Keep object files for LTO linking (don't delete them)
            # Object files are needed for proper LTO symbol resolution

            if show_progress:
                log_detail(f"Library '{library_name}' compiled successfully")

            return archive_file, object_files, compile_commands

        except subprocess.CalledProcessError as e:
            raise LibraryCompilationError(f"Compilation failed for library '{library_name}': {e}") from e
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise LibraryCompilationError(f"Failed to compile library '{library_name}': {e}") from e
