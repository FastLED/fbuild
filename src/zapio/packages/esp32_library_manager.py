"""ESP32 library dependency management.

This module handles downloading and compiling external libraries for ESP32 builds.
It uses the PlatformIO registry to resolve and download libraries, then compiles
them with the ESP32 toolchain.
"""

import json
import subprocess
from pathlib import Path
from typing import List, Optional

from zapio.packages.platformio_registry import (
    LibrarySpec,
    PlatformIORegistry,
    RegistryError,
)


class ESP32LibraryError(Exception):
    """Exception for ESP32 library management errors."""

    pass


class ESP32Library:
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
        return (
            self.lib_dir.exists() and self.src_dir.exists() and self.info_file.exists()
        )

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
        search_dir = (
            src_src if (src_src.exists() and src_src.is_dir()) else self.src_dir
        )

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
            return include_dirs

        # Check for src/src/ structure
        src_src = self.src_dir / "src"
        if src_src.exists() and src_src.is_dir():
            include_dirs.append(src_src)
        else:
            include_dirs.append(self.src_dir)

        # Look for additional include directories
        for name in ["include", "Include", "INCLUDE"]:
            inc_dir = self.lib_dir / name
            if inc_dir.exists():
                include_dirs.append(inc_dir)

        return include_dirs


class ESP32LibraryManager:
    """Manages ESP32 library dependencies."""

    def __init__(self, build_dir: Path, registry: Optional[PlatformIORegistry] = None):
        """Initialize library manager.

        Args:
            build_dir: Build directory (.zap/build/{board})
            registry: Optional registry client
        """
        self.build_dir = Path(build_dir)
        self.libs_dir = self.build_dir / "libs"
        self.registry = registry or PlatformIORegistry()

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

    def get_library(self, spec: LibrarySpec) -> ESP32Library:
        """Get a library instance for a specification.

        Args:
            spec: Library specification

        Returns:
            ESP32Library instance
        """
        lib_name = self._sanitize_name(spec.name)
        lib_dir = self.libs_dir / lib_name
        return ESP32Library(lib_dir, lib_name)

    def download_library(
        self, spec: LibrarySpec, show_progress: bool = True
    ) -> ESP32Library:
        """Download a library from PlatformIO registry.

        Args:
            spec: Library specification
            show_progress: Whether to show progress

        Returns:
            ESP32Library instance

        Raises:
            ESP32LibraryError: If download fails
        """
        try:
            library = self.get_library(spec)

            # Skip if already downloaded
            if library.exists:
                if show_progress:
                    print(f"Library '{spec.name}' already downloaded")
                return library

            # Download from registry
            self.registry.download_library(
                spec, library.lib_dir, show_progress=show_progress
            )

            return library

        except RegistryError as e:
            raise ESP32LibraryError(f"Failed to download library {spec}: {e}") from e

    def needs_rebuild(
        self, library: ESP32Library, compiler_flags: List[str]
    ) -> tuple[bool, str]:
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

        except Exception:
            return True, "Could not load build info"

        return False, ""

    def compile_library(
        self,
        library: ESP32Library,
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
            ESP32LibraryError: If compilation fails
        """
        try:
            if show_progress:
                print(f"Compiling library: {library.name}")

            # Get source files
            sources = library.get_source_files()
            if not sources:
                raise ESP32LibraryError(
                    f"No source files found in library '{library.name}'"
                )

            if show_progress:
                print(f"  Found {len(sources)} source file(s)")

            # Get library's own include directories
            lib_includes = library.get_include_dirs()
            all_includes = list(include_paths) + lib_includes

            # Compile each source file
            object_files = []
            gcc_path = toolchain_path / "riscv32-esp-elf-gcc"
            gxx_path = toolchain_path / "riscv32-esp-elf-g++"

            # Create response file for include paths (avoid Windows command line length limit)
            include_flags = [
                f"-I{str(inc).replace(chr(92), '/')}" for inc in all_includes
            ]
            response_file = library.lib_dir / "includes.rsp"
            with open(response_file, "w") as f:
                f.write("\n".join(include_flags))

            for source in sources:
                # Determine compiler based on extension
                if source.suffix in [".cpp", ".cc", ".cxx"]:
                    compiler = gxx_path
                else:
                    compiler = gcc_path

                # Output object file
                obj_file = library.lib_dir / f"{source.stem}.o"

                # Build compile command
                cmd = [str(compiler), "-c"]
                cmd.extend(compiler_flags)

                # Use response file for include paths
                cmd.append(f"@{response_file}")
                # Add source and output
                cmd.extend(["-o", str(obj_file), str(source)])

                # Compile
                if show_progress:
                    print(f"  Compiling {source.name}...")

                result = subprocess.run(
                    cmd, capture_output=True, text=True, encoding="utf-8"
                )

                if result.returncode != 0:
                    raise ESP32LibraryError(
                        f"Failed to compile {source.name}:\n{result.stderr}"
                    )

                object_files.append(obj_file)

            # Create static archive using ar
            ar_path = toolchain_path / "riscv32-esp-elf-ar"

            if show_progress:
                print(f"  Creating archive: {library.archive_file.name}")

            # Remove old archive if exists
            if library.archive_file.exists():
                library.archive_file.unlink()

            # Create new archive
            cmd = [str(ar_path), "rcs", str(library.archive_file)]
            cmd.extend([str(obj) for obj in object_files])

            result = subprocess.run(
                cmd, capture_output=True, text=True, encoding="utf-8"
            )

            if result.returncode != 0:
                raise ESP32LibraryError(
                    f"Failed to create archive for {library.name}:\n{result.stderr}"
                )

            # Save build info
            build_info = {
                "compiler_flags": compiler_flags,
                "source_count": len(sources),
                "object_files": [str(obj) for obj in object_files],
            }
            with open(library.build_info_file, "w", encoding="utf-8") as f:
                json.dump(build_info, f, indent=2)

            if show_progress:
                print(f"Library '{library.name}' compiled successfully")

            return library.archive_file

        except subprocess.CalledProcessError as e:
            raise ESP32LibraryError(
                f"Compilation failed for library '{library.name}': {e}"
            ) from e
        except Exception as e:
            raise ESP32LibraryError(
                f"Failed to compile library '{library.name}': {e}"
            ) from e

    def ensure_libraries(
        self,
        lib_specs: List[str],
        toolchain_path: Path,
        compiler_flags: List[str],
        include_paths: List[Path],
        show_progress: bool = True,
    ) -> List[ESP32Library]:
        """Ensure all library dependencies are downloaded and compiled.

        Args:
            lib_specs: List of library specification strings
            toolchain_path: Path to toolchain bin directory
            compiler_flags: Compiler flags
            include_paths: Include directories
            show_progress: Whether to show progress

        Returns:
            List of compiled ESP32Library instances
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
                if show_progress and reason:
                    print(f"Rebuilding library '{library.name}': {reason}")

                self.compile_library(
                    library,
                    toolchain_path,
                    compiler_flags,
                    include_paths,
                    show_progress,
                )
            else:
                if show_progress:
                    print(f"Library '{library.name}' is up to date")

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
                    library = ESP32Library(lib_dir, lib_dir.name)
                    if library.exists:
                        include_paths.extend(library.get_include_dirs())
        return include_paths
