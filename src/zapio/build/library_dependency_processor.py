"""
Library dependency processing for Zapio build system.

This module handles downloading, compiling, and managing library dependencies
from platformio.ini lib_deps configuration. It coordinates between the
library manager and the build system to ensure all dependencies are ready.
"""

from pathlib import Path
from typing import List, Optional

from ..config.board_config import BoardConfig
from ..packages.library_manager import LibraryManager
from ..packages.toolchain import ToolchainAVR


class LibraryDependencyProcessor:
    """
    Processes library dependencies for the build system.

    This class handles:
    - Downloading library dependencies from GitHub URLs
    - Compiling libraries with appropriate flags
    - Managing library include paths and object files
    - Coordinating between LibraryManager and build components

    Example usage:
        processor = LibraryDependencyProcessor(
            build_dir=Path(".zapio/build/uno"),
            mode="release",
            verbose=True
        )
        result = processor.process_dependencies(
            lib_deps=["https://github.com/user/library"],
            toolchain=toolchain,
            board_config=board_config,
            core_path=Path("~/.zapio/packages/arduino-avr-1.8.6")
        )
        include_paths = result.include_paths
        object_files = result.object_files
    """

    def __init__(
        self,
        build_dir: Path,
        mode: str = "release",
        verbose: bool = False
    ):
        """
        Initialize library dependency processor.

        Args:
            build_dir: Build directory for storing library artifacts
            mode: Build mode ('release' or 'debug')
            verbose: Enable verbose output
        """
        self.build_dir = build_dir
        self.mode = mode
        self.verbose = verbose
        self.library_manager: Optional[LibraryManager] = None

    def process_dependencies(
        self,
        lib_deps: List[str],
        toolchain: ToolchainAVR,
        board_config: BoardConfig,
        core_path: Path
    ) -> 'LibraryProcessingResult':
        """
        Process all library dependencies.

        Downloads, compiles, and prepares all library dependencies for linking.

        Args:
            lib_deps: List of library dependency URLs (e.g., GitHub URLs)
            toolchain: Toolchain instance for compilation
            board_config: Board configuration for compiler flags
            core_path: Arduino core installation path

        Returns:
            LibraryProcessingResult with include paths and object files

        Raises:
            LibraryError: If library processing fails
        """
        if not lib_deps:
            if self.verbose:
                print("      No library dependencies")
            return LibraryProcessingResult(
                libraries=[],
                include_paths=[],
                object_files=[]
            )

        if self.verbose:
            print(f"      Found {len(lib_deps)} library dependencies")

        # Initialize library manager
        self.library_manager = LibraryManager(self.build_dir, mode=self.mode)

        # Get compiler tools
        tools = toolchain.get_all_tools()

        # Prepare compilation parameters
        lib_defines = self._prepare_defines(board_config)
        lib_includes = board_config.get_include_paths(core_path)

        # Ensure all libraries are downloaded and compiled
        libraries = self.library_manager.ensure_libraries(
            lib_deps=lib_deps,
            compiler_path=tools['avr-gcc'],
            mcu=board_config.mcu,
            f_cpu=board_config.f_cpu,
            defines=lib_defines,
            include_paths=lib_includes,
            extra_flags=[],
            show_progress=self.verbose
        )

        # Get library artifacts
        lib_include_paths = self.library_manager.get_library_include_paths()
        lib_objects = self.library_manager.get_library_objects()

        if self.verbose:
            print(f"      Compiled {len(libraries)} libraries")
            print(f"      Library objects: {len(lib_objects)}")

        # Extract library directory paths from Library objects
        library_paths = [lib.lib_dir for lib in libraries]

        return LibraryProcessingResult(
            libraries=library_paths,
            include_paths=lib_include_paths,
            object_files=lib_objects
        )

    def _prepare_defines(self, board_config: BoardConfig) -> List[str]:
        """
        Prepare defines for library compilation.

        Converts board configuration defines dictionary to list format
        suitable for library compilation.

        Args:
            board_config: Board configuration

        Returns:
            List of define strings (e.g., ["F_CPU=16000000L", "ARDUINO=10819"])
        """
        lib_defines_dict = board_config.get_defines()
        lib_defines = []

        for key, value in lib_defines_dict.items():
            if value:
                lib_defines.append(f"{key}={value}")
            else:
                lib_defines.append(key)

        return lib_defines


class LibraryProcessingResult:
    """
    Result of library dependency processing.

    Contains all information needed by the build system about processed libraries.
    """

    def __init__(
        self,
        libraries: List[Path],
        include_paths: List[Path],
        object_files: List[Path]
    ):
        """
        Initialize library processing result.

        Args:
            libraries: List of library root directories
            include_paths: List of include directories to add to compiler
            object_files: List of compiled object files to link
        """
        self.libraries = libraries
        self.include_paths = include_paths
        self.object_files = object_files
