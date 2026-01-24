"""ESP32 library dependency management.

This module handles downloading and compiling external libraries for ESP32 builds.
It uses the PlatformIO registry to resolve and download libraries, then compiles
them with the ESP32 toolchain.
"""

import json
import logging
import platform
import subprocess
from pathlib import Path
from typing import List, Optional

from fbuild.output import log_detail
from fbuild.packages.platformio_registry import (
    LibrarySpec,
    PlatformIORegistry,
    RegistryError,
)
from fbuild.subprocess_utils import safe_run

logger = logging.getLogger(__name__)


class LibraryErrorESP32(Exception):
    """Exception for ESP32 library management errors."""

    pass


class LibraryESP32:
    """Represents a downloaded and compiled ESP32 library."""

    def __init__(self, lib_dir: Path, name: str):
        """Initialize ESP32 library.

        Args:
            lib_dir: Root directory for the library
            name: Library name
        """
        self.lib_dir = lib_dir
        self.name = name
        self.src_dir = lib_dir / "src"
        self.info_file = lib_dir / "library.json"
        self.archive_file = lib_dir / f"lib{name}.a"
        self.build_info_file = lib_dir / "build_info.json"

    @property
    def exists(self) -> bool:
        """Check if library is downloaded and compiled."""
        return self.lib_dir.exists() and self.src_dir.exists() and self.info_file.exists()

    @property
    def is_compiled(self) -> bool:
        """Check if library is compiled."""
        return self.archive_file.exists() and self.build_info_file.exists()

    def get_source_files(self) -> List[Path]:
        """Find all source files (.c, .cpp, .cc, .cxx) in the library.

        Returns:
            List of source file paths
        """
        if not self.src_dir.exists():
            return []

        sources = []

        # Check for src/src/ structure (some libraries have this)
        src_src = self.src_dir / "src"
        search_dir = src_src if (src_src.exists() and src_src.is_dir()) else self.src_dir

        # Find all source files recursively
        for pattern in ["**/*.c", "**/*.cpp", "**/*.cc", "**/*.cxx"]:
            for path in search_dir.glob(pattern):
                # Skip examples and tests (check relative path only)
                rel_path = str(path.relative_to(search_dir)).lower()
                if "example" not in rel_path and "test" not in rel_path:
                    sources.append(path)

        return sources

    def get_include_dirs(self) -> List[Path]:
        """Get include directories for this library.

        Returns:
            List of include directory paths
        """
        include_dirs = []

        if not self.src_dir.exists():
            logger.warning(f"[INCLUDE_DEBUG] src_dir does not exist: {self.src_dir}")
            return include_dirs

        # Check for src/src/ structure
        src_src = self.src_dir / "src"
        if src_src.exists() and src_src.is_dir():
            logger.debug(f"[INCLUDE_DEBUG] Found nested src/, adding: {src_src}")
            include_dirs.append(src_src)
        else:
            logger.debug(f"[INCLUDE_DEBUG] No nested src/, adding base: {self.src_dir}")
            include_dirs.append(self.src_dir)

        # Look for additional include directories
        for name in ["include", "Include", "INCLUDE"]:
            inc_dir = self.lib_dir / name
            if inc_dir.exists():
                logger.debug(f"[INCLUDE_DEBUG] Found additional include dir: {inc_dir}")
                include_dirs.append(inc_dir)

        logger.debug(f"[INCLUDE_DEBUG] Final include_dirs for {self.name}: {include_dirs}")
        return include_dirs


class LibraryManagerESP32:
    """Manages ESP32 library dependencies."""

    def __init__(self, build_dir: Path, registry: Optional[PlatformIORegistry] = None, project_dir: Optional[Path] = None):
        """Initialize library manager.

        Args:
            build_dir: Build directory (.fbuild/build/{board})
            registry: Optional registry client
            project_dir: Optional project directory (for resolving relative local library paths)
        """
        self.build_dir = Path(build_dir)
        self.libs_dir = self.build_dir / "libs"
        self.registry = registry or PlatformIORegistry()
        self.project_dir = Path(project_dir) if project_dir else None

        # Ensure libs directory exists
        self.libs_dir.mkdir(parents=True, exist_ok=True)

    def _sanitize_name(self, name: str) -> str:
        """Sanitize library name for filesystem.

        Args:
            name: Library name

        Returns:
            Sanitized name
        """
        return name.lower().replace("/", "_").replace("@", "_")

    def _find_toolchain_compilers(self, toolchain_path: Path) -> tuple[Path, Path]:
        """Find GCC and G++ compilers in the toolchain directory.

        ESP32 uses different toolchains depending on the MCU architecture:
        - Xtensa (ESP32, ESP32-S2, ESP32-S3): xtensa-esp32-elf-gcc, xtensa-esp32-elf-g++
        - RISC-V (ESP32-C3, C6, H2): riscv32-esp-elf-gcc, riscv32-esp-elf-g++

        This method auto-detects which toolchain is available.

        Args:
            toolchain_path: Path to toolchain bin directory

        Returns:
            Tuple of (gcc_path, gxx_path)

        Raises:
            LibraryErrorESP32: If no suitable compiler is found
        """
        # Check for Xtensa toolchain first (more common for ESP32)
        exe_suffix = ".exe" if platform.system() == "Windows" else ""

        # Xtensa toolchain (ESP32, S2, S3)
        xtensa_gcc = toolchain_path / f"xtensa-esp32-elf-gcc{exe_suffix}"
        xtensa_gxx = toolchain_path / f"xtensa-esp32-elf-g++{exe_suffix}"

        if xtensa_gcc.exists() and xtensa_gxx.exists():
            logger.debug(f"[TOOLCHAIN] Using Xtensa toolchain: {xtensa_gcc}")
            return xtensa_gcc, xtensa_gxx

        # RISC-V toolchain (ESP32-C3, C6, H2)
        riscv_gcc = toolchain_path / f"riscv32-esp-elf-gcc{exe_suffix}"
        riscv_gxx = toolchain_path / f"riscv32-esp-elf-g++{exe_suffix}"

        if riscv_gcc.exists() and riscv_gxx.exists():
            logger.debug(f"[TOOLCHAIN] Using RISC-V toolchain: {riscv_gcc}")
            return riscv_gcc, riscv_gxx

        # Fallback: try to find any gcc/g++ pattern
        gcc_files = list(toolchain_path.glob(f"*-gcc{exe_suffix}"))
        gxx_files = list(toolchain_path.glob(f"*-g++{exe_suffix}"))

        if gcc_files and gxx_files:
            logger.debug(f"[TOOLCHAIN] Using discovered toolchain: {gcc_files[0]}")
            return gcc_files[0], gxx_files[0]

        raise LibraryErrorESP32(f"No suitable ESP32 toolchain found in {toolchain_path}. Expected xtensa-esp32-elf-gcc or riscv32-esp-elf-gcc")

    def _find_toolchain_ar(self, toolchain_path: Path) -> Path:
        """Find ar archiver in the toolchain directory.

        Args:
            toolchain_path: Path to toolchain bin directory

        Returns:
            Path to ar binary

        Raises:
            LibraryErrorESP32: If no suitable ar is found
        """
        exe_suffix = ".exe" if platform.system() == "Windows" else ""

        # Check for Xtensa ar
        xtensa_ar = toolchain_path / f"xtensa-esp32-elf-ar{exe_suffix}"
        if xtensa_ar.exists():
            return xtensa_ar

        # Check for RISC-V ar
        riscv_ar = toolchain_path / f"riscv32-esp-elf-ar{exe_suffix}"
        if riscv_ar.exists():
            return riscv_ar

        # Fallback: try to find any ar pattern
        ar_files = list(toolchain_path.glob(f"*-ar{exe_suffix}"))
        if ar_files:
            return ar_files[0]

        raise LibraryErrorESP32(f"No suitable ar archiver found in {toolchain_path}. Expected xtensa-esp32-elf-ar or riscv32-esp-elf-ar")

    def get_library(self, spec: LibrarySpec) -> LibraryESP32:
        """Get a library instance for a specification.

        Args:
            spec: Library specification

        Returns:
            LibraryESP32 instance
        """
        lib_name = self._sanitize_name(spec.name)
        lib_dir = self.libs_dir / lib_name
        return LibraryESP32(lib_dir, lib_name)

    def _handle_local_library(self, spec: LibrarySpec, show_progress: bool = True) -> LibraryESP32:
        """Handle a local library specification (file:// or relative path).

        Args:
            spec: Library specification with is_local=True
            show_progress: Whether to show progress

        Returns:
            LibraryESP32 instance

        Raises:
            LibraryErrorESP32: If local library setup fails
        """
        import logging

        logger = logging.getLogger(__name__)

        logger.debug(f"[LOCAL_LIB] Step 1: Starting _handle_local_library for spec: {spec}")

        if not spec.local_path:
            raise LibraryErrorESP32(f"Local library spec has no path: {spec}")

        logger.debug(f"[LOCAL_LIB] Step 2: spec.local_path = {spec.local_path}")

        library = self.get_library(spec)
        logger.debug(f"[LOCAL_LIB] Step 3: Created library instance, lib_dir = {library.lib_dir}")

        # Skip if already set up
        if library.exists:
            logger.debug("[LOCAL_LIB] Step 4: Library already exists, returning early")
            if show_progress:
                log_detail(f"Local library '{spec.name}' already set up")
            return library

        logger.debug("[LOCAL_LIB] Step 5: Library doesn't exist, need to set up")

        # Resolve the local path (relative to project directory if available, otherwise cwd)
        local_path = spec.local_path
        logger.debug(f"[LOCAL_LIB] Step 6: local_path before absolute check: {local_path}, is_absolute={local_path.is_absolute()}")

        if not local_path.is_absolute():
            # Make absolute relative to project directory (where platformio.ini is)
            # If project_dir not set, fall back to current working directory
            base_dir = self.project_dir if self.project_dir else Path.cwd()
            logger.debug(f"[LOCAL_LIB] Step 7: Converting to absolute, base_dir = {base_dir}")
            local_path = base_dir / local_path
            logger.debug(f"[LOCAL_LIB] Step 8: After joining: local_path = {local_path}")

        # Normalize path (resolve .. and .)
        logger.debug(f"[LOCAL_LIB] Step 9: Before resolve(), local_path = {local_path}")
        local_path = local_path.resolve()
        logger.debug(f"[LOCAL_LIB] Step 10: After resolve(), local_path = {local_path}")

        # Verify library exists
        logger.debug(f"[LOCAL_LIB] Step 11: Checking if local_path exists: {local_path}")
        if not local_path.exists():
            raise LibraryErrorESP32(f"Local library path does not exist: {local_path}")

        logger.debug("[LOCAL_LIB] Step 12: Path exists, checking if directory")
        if not local_path.is_dir():
            raise LibraryErrorESP32(f"Local library path is not a directory: {local_path}")

        logger.debug("[LOCAL_LIB] Step 13: Checking for library.json or library.properties")
        # Look for library.json (Arduino library metadata)
        library_json = local_path / "library.json"
        library_properties = local_path / "library.properties"

        if not library_json.exists() and not library_properties.exists():
            # Check if there's a src subdirectory (common Arduino structure)
            logger.debug("[LOCAL_LIB] Step 14: No metadata files, checking for src/ directory")
            src_dir = local_path / "src"
            if not src_dir.exists():
                raise LibraryErrorESP32(f"Local library has no library.json, library.properties, or src/ directory: {local_path}")

        logger.debug("[LOCAL_LIB] Step 15: Library structure validated")
        if show_progress:
            log_detail(f"Setting up local library '{spec.name}' from {local_path}")

        # Create library directory structure
        logger.debug(f"[LOCAL_LIB] Step 16: Creating library directory: {library.lib_dir}")
        library.lib_dir.mkdir(parents=True, exist_ok=True)

        # Create a symlink or copy to the source directory
        # On Windows, force copy instead of symlink due to MSYS/cross-compiler incompatibility
        # MSYS creates /c/Users/... symlinks that ESP32 cross-compiler can't follow when
        # include paths use Windows format (C:/Users/...)
        import os
        import shutil

        is_windows = platform.system() == "Windows"
        logger.debug(f"[LOCAL_LIB] Step 17: Platform detected: {platform.system()} (is_windows={is_windows})")

        # Check if local_path has a src/ subdirectory (Arduino library structure)
        # If so, copy from local_path/src instead of local_path to avoid path duplication
        source_path = local_path / "src" if (local_path / "src").is_dir() else local_path
        logger.debug(f"[LOCAL_LIB] Step 17.5: Source path for copy/symlink: {source_path}")

        if is_windows:
            # Windows: Always copy to avoid MSYS symlink issues with ESP32 cross-compiler
            logger.debug("[LOCAL_LIB] Step 18: Windows detected, forcing copy (no symlink)")
            if show_progress:
                log_detail(f"Copying library files from {source_path}...", indent=8)

            if library.src_dir.exists():
                logger.debug("[LOCAL_LIB] Step 19: Removing existing src_dir before copy")
                shutil.rmtree(library.src_dir)

            # Define ignore function to exclude build artifacts and version control
            # This prevents recursive .fbuild directories and other unnecessary files
            def ignore_build_artifacts(directory: str, contents: list[str]) -> list[str]:
                ignored = []
                for name in contents:
                    if name in {".fbuild", ".pio", ".git", ".venv", "__pycache__", ".pytest_cache", "node_modules", ".cache", "build", ".build", ".vscode", ".idea"}:
                        ignored.append(name)
                        logger.debug(f"[LOCAL_LIB] Ignoring: {os.path.join(directory, name)}")
                return ignored

            logger.debug("[LOCAL_LIB] Step 20: Calling shutil.copytree() with symlinks=False and ignore filter")
            # symlinks=False: Dereference any symlinks in source tree (important for nested dependencies)
            # ignore: Skip build artifacts, version control, and cache directories
            shutil.copytree(source_path, library.src_dir, symlinks=False, ignore=ignore_build_artifacts)
            logger.debug("[LOCAL_LIB] Step 21: Copy completed successfully")
            if show_progress:
                log_detail(f"Copied library files to {library.src_dir}", indent=8)
        else:
            # Unix: Use symlink for efficiency (actual files stay in original location)
            logger.debug(f"[LOCAL_LIB] Step 18: Unix platform, attempting symlink from {library.src_dir} to {source_path}")
            try:
                # Try to create symlink first (faster, no disk space duplication)
                if library.src_dir.exists():
                    logger.debug("[LOCAL_LIB] Step 19: Removing existing src_dir")
                    library.src_dir.unlink()
                logger.debug("[LOCAL_LIB] Step 20: Calling os.symlink()")
                os.symlink(str(source_path), str(library.src_dir), target_is_directory=True)
                logger.debug("[LOCAL_LIB] Step 21: Symlink created successfully")
                if show_progress:
                    log_detail(f"Created symlink to {source_path}", indent=8)
            except OSError as e:
                # Symlink failed (maybe no permissions), fall back to copying
                logger.debug(f"[LOCAL_LIB] Step 22: Symlink failed with error: {e}, falling back to copy")
                if show_progress:
                    log_detail("Symlink failed, copying library files...", indent=8)

                if library.src_dir.exists():
                    logger.debug("[LOCAL_LIB] Step 23: Removing existing src_dir before copy")
                    shutil.rmtree(library.src_dir)

                # Define ignore function to exclude build artifacts and version control
                def ignore_build_artifacts(directory: str, contents: list[str]) -> list[str]:
                    ignored = []
                    for name in contents:
                        if name in {".fbuild", ".pio", ".git", ".venv", "__pycache__", ".pytest_cache", "node_modules", ".cache", "build", ".build", ".vscode", ".idea"}:
                            ignored.append(name)
                            logger.debug(f"[LOCAL_LIB] Ignoring: {os.path.join(directory, name)}")
                    return ignored

                logger.debug("[LOCAL_LIB] Step 24: Calling shutil.copytree() with symlinks=False and ignore filter")
                shutil.copytree(source_path, library.src_dir, symlinks=False, ignore=ignore_build_artifacts)
                logger.debug("[LOCAL_LIB] Step 25: Copy completed successfully")
                if show_progress:
                    log_detail(f"Copied library files to {library.src_dir}", indent=8)

        # Create library.json metadata
        import json

        logger.debug("[LOCAL_LIB] Step 25: Creating library.json metadata")

        lib_name = spec.name
        lib_version = "local"

        # Try to read version from existing metadata
        logger.debug(f"[LOCAL_LIB] Step 26: Checking if source library.json exists: {library_json}")
        if library_json.exists():
            logger.debug("[LOCAL_LIB] Step 27: Reading metadata from source library.json")
            try:
                with open(library_json, "r", encoding="utf-8") as f:
                    metadata = json.load(f)
                    lib_name = metadata.get("name", spec.name)
                    lib_version = metadata.get("version", "local")
                logger.debug(f"[LOCAL_LIB] Step 28: Read metadata: name={lib_name}, version={lib_version}")
            except KeyboardInterrupt as ke:
                from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

                handle_keyboard_interrupt_properly(ke)
                raise  # Never reached
            except Exception as e:
                logger.debug(f"[LOCAL_LIB] Step 29: Failed to read metadata: {e}, using defaults")
                pass  # Use defaults

        # Save library info in the expected location
        info_file = library.lib_dir / "library.json"
        logger.debug(f"[LOCAL_LIB] Step 30: Writing library info to: {info_file}")
        with open(info_file, "w", encoding="utf-8") as f:
            json.dump(
                {
                    "name": lib_name,
                    "owner": "local",
                    "version": lib_version,
                    "local_path": str(local_path),
                    "is_local": True,
                },
                f,
                indent=2,
            )

        logger.debug("[LOCAL_LIB] Step 31: Local library setup completed successfully")
        if show_progress:
            log_detail(f"Local library '{spec.name}' set up successfully")

        return library

    def download_library(self, spec: LibrarySpec, show_progress: bool = True) -> LibraryESP32:
        """Download a library from PlatformIO registry or set up local library.

        Args:
            spec: Library specification
            show_progress: Whether to show progress

        Returns:
            LibraryESP32 instance

        Raises:
            LibraryErrorESP32: If download or setup fails
        """
        try:
            # Check if this is a local library first
            if spec.is_local:
                return self._handle_local_library(spec, show_progress)

            # Remote library - use existing registry logic
            library = self.get_library(spec)

            # Skip if already downloaded
            if library.exists:
                if show_progress:
                    log_detail(f"Library '{spec.name}' already downloaded (cached)")
                return library

            # Download from registry
            self.registry.download_library(spec, library.lib_dir, show_progress=show_progress)

            return library

        except RegistryError as e:
            raise LibraryErrorESP32(f"Failed to download library {spec}: {e}") from e

    def needs_rebuild(self, library: LibraryESP32, compiler_flags: List[str]) -> tuple[bool, str]:
        """Check if a library needs to be rebuilt.

        Args:
            library: Library to check
            compiler_flags: Current compiler flags

        Returns:
            Tuple of (needs_rebuild, reason)
        """
        if not library.archive_file.exists():
            return True, "Archive not found"

        if not library.build_info_file.exists():
            return True, "Build info missing"

        try:
            with open(library.build_info_file, "r", encoding="utf-8") as f:
                build_info = json.load(f)

            # Check if compiler flags changed
            stored_flags = build_info.get("compiler_flags", [])
            if stored_flags != compiler_flags:
                return True, "Compiler flags changed"

        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception:
            return True, "Could not load build info"

        return False, ""

    def compile_library(
        self,
        library: LibraryESP32,
        toolchain_path: Path,
        compiler_flags: List[str],
        include_paths: List[Path],
        show_progress: bool = True,
    ) -> Path:
        """Compile a library into a static archive.

        Args:
            library: Library to compile
            toolchain_path: Path to toolchain bin directory
            compiler_flags: Compiler flags
            include_paths: Include directories
            show_progress: Whether to show progress

        Returns:
            Path to compiled archive file

        Raises:
            LibraryErrorESP32: If compilation fails
        """
        try:
            log_detail(f"Compiling library: {library.name}")

            # Get source files
            sources = library.get_source_files()
            if not sources:
                raise LibraryErrorESP32(f"No source files found in library '{library.name}'")

            log_detail(f"Found {len(sources)} source file(s)", indent=8)

            # Get library's own include directories
            lib_includes = library.get_include_dirs()
            all_includes = list(include_paths) + lib_includes

            # Compile each source file
            object_files = []

            # Auto-detect toolchain prefix from available binaries
            # ESP32/S2/S3 use xtensa, C3/C6/H2 use RISC-V
            gcc_path, gxx_path = self._find_toolchain_compilers(toolchain_path)

            # Create response file for include paths (avoid Windows command line length limit)
            include_flags = [f"-I{str(inc).replace(chr(92), '/')}" for inc in all_includes]
            response_file = library.lib_dir / "includes.rsp"
            with open(response_file, "w") as f:
                f.write("\n".join(include_flags))

            for source in sources:
                # Determine compiler based on extension
                if source.suffix in [".cpp", ".cc", ".cxx"]:
                    compiler = gxx_path
                else:
                    compiler = gcc_path

                # Output object file - maintain directory structure relative to src_dir
                # This prevents name collisions and keeps .d files organized
                rel_path = source.relative_to(library.src_dir)
                obj_file = library.src_dir / rel_path.with_suffix(".o")

                # Ensure output directory exists
                obj_file.parent.mkdir(parents=True, exist_ok=True)

                # Build compile command
                cmd = [str(compiler), "-c"]
                cmd.extend(compiler_flags)

                # Use response file for include paths
                cmd.append(f"@{response_file}")
                # Add source and output
                cmd.extend(["-o", str(obj_file), str(source)])

                # Compile
                if show_progress:
                    log_detail(f"Compiling {source.name}...", indent=8)

                result = safe_run(cmd, capture_output=True, text=True, encoding="utf-8")

                if result.returncode != 0:
                    raise LibraryErrorESP32(f"Failed to compile {source.name}:\n{result.stderr}")

                object_files.append(obj_file)

            # Create static archive using ar
            ar_path = self._find_toolchain_ar(toolchain_path)

            log_detail(f"Creating archive: {library.archive_file.name}", indent=8)

            # Remove old archive if exists
            if library.archive_file.exists():
                library.archive_file.unlink()

            # Create new archive
            cmd = [str(ar_path), "rcs", str(library.archive_file)]
            cmd.extend([str(obj) for obj in object_files])

            result = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8")

            if result.returncode != 0:
                raise LibraryErrorESP32(f"Failed to create archive for {library.name}:\n{result.stderr}")

            # Save build info
            build_info = {
                "compiler_flags": compiler_flags,
                "source_count": len(sources),
                "object_files": [str(obj) for obj in object_files],
            }
            with open(library.build_info_file, "w", encoding="utf-8") as f:
                json.dump(build_info, f, indent=2)

            log_detail(f"Library '{library.name}' compiled successfully")

            return library.archive_file

        except subprocess.CalledProcessError as e:
            raise LibraryErrorESP32(f"Compilation failed for library '{library.name}': {e}") from e
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise LibraryErrorESP32(f"Failed to compile library '{library.name}': {e}") from e

    def ensure_libraries(
        self,
        lib_specs: List[str],
        toolchain_path: Path,
        compiler_flags: List[str],
        include_paths: List[Path],
        show_progress: bool = True,
    ) -> List[LibraryESP32]:
        """Ensure all library dependencies are downloaded and compiled.

        Args:
            lib_specs: List of library specification strings
            toolchain_path: Path to toolchain bin directory
            compiler_flags: Compiler flags
            include_paths: Include directories
            show_progress: Whether to show progress

        Returns:
            List of compiled LibraryESP32 instances
        """
        libraries = []

        for spec_str in lib_specs:
            # Parse library specification
            spec = LibrarySpec.parse(spec_str)

            # Download library
            library = self.download_library(spec, show_progress)

            # Check if rebuild needed
            needs_rebuild, reason = self.needs_rebuild(library, compiler_flags)

            if needs_rebuild:
                if reason:
                    log_detail(f"Rebuilding library '{library.name}': {reason}")

                self.compile_library(
                    library,
                    toolchain_path,
                    compiler_flags,
                    include_paths,
                    show_progress,
                )
            else:
                log_detail(f"Library '{library.name}' is up to date (cached)")

            libraries.append(library)

        return libraries

    def get_library_archives(self) -> List[Path]:
        """Get paths to all compiled library archives.

        Returns:
            List of .a archive file paths
        """
        archives = []
        if self.libs_dir.exists():
            for lib_dir in self.libs_dir.iterdir():
                if lib_dir.is_dir():
                    archive = lib_dir / f"lib{lib_dir.name}.a"
                    if archive.exists():
                        archives.append(archive)
        return archives

    def get_library_include_paths(self) -> List[Path]:
        """Get all include paths from downloaded libraries.

        Returns:
            List of include directory paths
        """
        include_paths = []
        if self.libs_dir.exists():
            for lib_dir in self.libs_dir.iterdir():
                if lib_dir.is_dir():
                    library = LibraryESP32(lib_dir, lib_dir.name)
                    if library.exists:
                        include_paths.extend(library.get_include_dirs())
        return include_paths
