"""
ESP32-specific build orchestration for Zapio projects.

This module handles ESP32 platform builds separately from AVR builds,
providing cleaner separation of concerns and better maintainability.
"""

import time
from pathlib import Path
from typing import List, Optional
from dataclasses import dataclass

from ..packages import Cache
from ..packages.esp32_platform import ESP32Platform
from ..packages.esp32_toolchain import ESP32Toolchain
from ..packages.esp32_framework import ESP32Framework
from ..packages.esp32_library_manager import ESP32LibraryManager
from .configurable_compiler import ConfigurableCompiler
from .configurable_linker import ConfigurableLinker
from .linker import SizeInfo


@dataclass
class ESP32BuildResult:
    """Result of an ESP32 build operation."""

    success: bool
    firmware_bin: Optional[Path]
    firmware_elf: Optional[Path]
    bootloader_bin: Optional[Path]
    partitions_bin: Optional[Path]
    size_info: Optional[SizeInfo]
    build_time: float
    message: str


class ESP32Orchestrator:
    """
    Orchestrates ESP32-specific build process.

    Handles platform initialization, toolchain setup, framework preparation,
    library compilation, and firmware generation for ESP32 targets.
    """

    def __init__(self, cache: Cache, verbose: bool = False):
        """
        Initialize ESP32 orchestrator.

        Args:
            cache: Cache instance for package management
            verbose: Enable verbose output
        """
        self.cache = cache
        self.verbose = verbose

    def build(
        self,
        project_dir: Path,
        env_name: str,
        board_id: str,
        env_config: dict,
        build_flags: List[str],
        clean: bool = False
    ) -> ESP32BuildResult:
        """
        Execute complete ESP32 build process.

        Args:
            project_dir: Project directory
            env_name: Environment name
            board_id: Board ID (e.g., esp32-c6-devkitm-1)
            env_config: Environment configuration dict
            build_flags: User build flags from platformio.ini
            clean: Whether to clean before build

        Returns:
            ESP32BuildResult with build status and output paths
        """
        start_time = time.time()

        try:
            # Get platform URL from env_config
            platform_url = env_config.get('platform')
            if not platform_url:
                return self._error_result(
                    start_time,
                    "No platform URL specified in platformio.ini"
                )

            # Initialize platform
            if self.verbose:
                print("[3/10] Initializing ESP32 platform...")

            platform = ESP32Platform(self.cache, platform_url, show_progress=self.verbose)
            platform.ensure_platform()

            # Get board configuration
            board_json = platform.get_board_json(board_id)
            mcu = board_json.get("build", {}).get("mcu", "esp32c6")

            if self.verbose:
                print(f"      Board: {board_id}")
                print(f"      MCU: {mcu}")

            # Get required packages
            packages = platform.get_required_packages(mcu)

            # Initialize toolchain
            toolchain = self._setup_toolchain(packages, start_time)
            if toolchain is None:
                return self._error_result(
                    start_time,
                    "Failed to initialize toolchain"
                )

            # Initialize framework
            framework = self._setup_framework(packages, start_time)
            if framework is None:
                return self._error_result(
                    start_time,
                    "Failed to initialize framework"
                )

            # Setup build directory
            build_dir = self._setup_build_directory(env_name, clean)

            # Initialize compiler
            if self.verbose:
                print("[7/10] Compiling Arduino core...")

            compiler = ConfigurableCompiler(
                platform,
                toolchain,
                framework,
                board_id,
                build_dir,
                platform_config=None,
                show_progress=self.verbose,
                user_build_flags=build_flags
            )

            # Compile Arduino core
            core_obj_files = compiler.compile_core()
            core_archive = compiler.create_core_archive(core_obj_files)

            if self.verbose:
                print(f"      Compiled {len(core_obj_files)} core source files")

            # Handle library dependencies
            library_archives, library_include_paths = self._process_libraries(
                env_config, build_dir, compiler, toolchain
            )

            # Add library include paths to compiler
            if library_include_paths:
                compiler.add_library_includes(library_include_paths)

            # Find and compile sketch
            sketch_obj_files = self._compile_sketch(project_dir, compiler, start_time)
            if sketch_obj_files is None:
                return self._error_result(
                    start_time,
                    f"No .ino sketch file found in {project_dir}"
                )

            # Initialize linker
            if self.verbose:
                print("[9/10] Linking firmware...")

            linker = ConfigurableLinker(
                platform,
                toolchain,
                framework,
                board_id,
                build_dir,
                platform_config=None,
                show_progress=self.verbose
            )

            # Link firmware
            firmware_elf = linker.link(sketch_obj_files, core_archive, library_archives=library_archives)

            # Generate binary
            if self.verbose:
                print("[10/10] Generating firmware binary...")

            firmware_bin = linker.generate_bin(firmware_elf)

            # Generate bootloader and partition table
            bootloader_bin, partitions_bin = self._generate_boot_components(
                linker, mcu
            )

            build_time = time.time() - start_time

            if self.verbose:
                self._print_success(
                    build_time, firmware_elf, firmware_bin,
                    bootloader_bin, partitions_bin
                )

            return ESP32BuildResult(
                success=True,
                firmware_bin=firmware_bin,
                firmware_elf=firmware_elf,
                bootloader_bin=bootloader_bin,
                partitions_bin=partitions_bin,
                size_info=None,  # TODO: Add size info for ESP32
                build_time=build_time,
                message="Build successful (native ESP32 build)"
            )

        except KeyboardInterrupt as ke:
            from zapio.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            build_time = time.time() - start_time
            import traceback
            error_trace = traceback.format_exc()
            return ESP32BuildResult(
                success=False,
                firmware_bin=None,
                firmware_elf=None,
                bootloader_bin=None,
                partitions_bin=None,
                size_info=None,
                build_time=build_time,
                message=f"ESP32 native build failed: {e}\n\n{error_trace}"
            )

    def _setup_toolchain(
        self,
        packages: dict,
        start_time: float
    ) -> Optional[ESP32Toolchain]:
        """
        Initialize ESP32 toolchain.

        Args:
            packages: Package URLs dictionary
            start_time: Build start time for error reporting

        Returns:
            ESP32Toolchain instance or None on failure
        """
        if self.verbose:
            print("[4/10] Initializing ESP32 toolchain...")

        toolchain_url = packages.get("toolchain-riscv32-esp") or packages.get("toolchain-xtensa-esp-elf")
        if not toolchain_url:
            return None

        # Determine toolchain type
        toolchain_type = "riscv32-esp" if "riscv32" in toolchain_url else "xtensa-esp-elf"
        toolchain = ESP32Toolchain(
            self.cache,
            toolchain_url,
            toolchain_type,
            show_progress=self.verbose
        )
        toolchain.ensure_toolchain()
        return toolchain

    def _setup_framework(
        self,
        packages: dict,
        start_time: float
    ) -> Optional[ESP32Framework]:
        """
        Initialize ESP32 framework.

        Args:
            packages: Package URLs dictionary
            start_time: Build start time for error reporting

        Returns:
            ESP32Framework instance or None on failure
        """
        if self.verbose:
            print("[5/10] Initializing ESP32 framework...")

        framework_url = packages.get("framework-arduinoespressif32")
        libs_url = packages.get("framework-arduinoespressif32-libs")

        if not framework_url or not libs_url:
            return None

        framework = ESP32Framework(
            self.cache,
            framework_url,
            libs_url,
            show_progress=self.verbose
        )
        framework.ensure_framework()
        return framework

    def _setup_build_directory(self, env_name: str, clean: bool) -> Path:
        """
        Setup build directory with optional cleaning.

        Args:
            env_name: Environment name
            clean: Whether to clean before build

        Returns:
            Build directory path
        """
        build_dir = self.cache.get_build_dir(env_name)

        if clean and build_dir.exists():
            if self.verbose:
                print("[6/10] Cleaning build directory...")
            import shutil
            shutil.rmtree(build_dir)

        build_dir.mkdir(parents=True, exist_ok=True)
        return build_dir

    def _process_libraries(
        self,
        env_config: dict,
        build_dir: Path,
        compiler: ConfigurableCompiler,
        toolchain: ESP32Toolchain
    ) -> tuple[List[Path], List[Path]]:
        """
        Process and compile library dependencies.

        Args:
            env_config: Environment configuration
            build_dir: Build directory
            compiler: Configured compiler instance
            toolchain: ESP32 toolchain instance

        Returns:
            Tuple of (library_archives, library_include_paths)
        """
        lib_deps = env_config.get('lib_deps', '')
        library_archives = []
        library_include_paths = []

        if not lib_deps:
            return library_archives, library_include_paths

        if self.verbose:
            print("[7.5/10] Processing library dependencies...")

        # Parse lib_deps (can be string or list)
        if isinstance(lib_deps, str):
            lib_specs = [dep.strip() for dep in lib_deps.split('\n') if dep.strip()]
        else:
            lib_specs = lib_deps

        if not lib_specs:
            return library_archives, library_include_paths

        # Initialize library manager
        lib_manager = ESP32LibraryManager(build_dir)

        # Get compiler flags for library compilation
        lib_compiler_flags = compiler.get_base_flags()

        # Get include paths for library compilation
        lib_include_paths = compiler.get_include_paths()

        # Get toolchain bin path
        toolchain_bin_path = toolchain.get_bin_path()
        if toolchain_bin_path is None:
            if self.verbose:
                print("      Warning: Toolchain bin directory not found, skipping libraries")
            return library_archives, library_include_paths

        # Ensure libraries are downloaded and compiled
        libraries = lib_manager.ensure_libraries(
            lib_specs,
            toolchain_bin_path,
            lib_compiler_flags,
            lib_include_paths,
            show_progress=self.verbose
        )

        # Get library archives and include paths
        library_archives = [lib.archive_file for lib in libraries if lib.is_compiled]
        library_include_paths = lib_manager.get_library_include_paths()

        if self.verbose:
            print(f"      Compiled {len(libraries)} library dependencies")

        return library_archives, library_include_paths

    def _compile_sketch(
        self,
        project_dir: Path,
        compiler: ConfigurableCompiler,
        start_time: float
    ) -> Optional[List[Path]]:
        """
        Find and compile sketch files.

        Args:
            project_dir: Project directory
            compiler: Configured compiler instance
            start_time: Build start time for error reporting

        Returns:
            List of compiled object files or None if no sketch found
        """
        if self.verbose:
            print("[8/10] Compiling sketch...")

        # Look for .ino files in the project directory
        sketch_files = list(project_dir.glob("*.ino"))
        if not sketch_files:
            return None

        sketch_path = sketch_files[0]
        sketch_obj_files = compiler.compile_sketch(sketch_path)

        if self.verbose:
            print(f"      Compiled {len(sketch_obj_files)} sketch file(s)")

        return sketch_obj_files

    def _generate_boot_components(
        self,
        linker: ConfigurableLinker,
        mcu: str
    ) -> tuple[Optional[Path], Optional[Path]]:
        """
        Generate bootloader and partition table for ESP32.

        Args:
            linker: Configured linker instance
            mcu: MCU identifier

        Returns:
            Tuple of (bootloader_bin, partitions_bin)
        """
        bootloader_bin = None
        partitions_bin = None

        if not mcu.startswith("esp32"):
            return bootloader_bin, partitions_bin

        if self.verbose:
            print("[11/12] Generating bootloader...")
        try:
            bootloader_bin = linker.generate_bootloader()
        except KeyboardInterrupt as ke:
            from zapio.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            if self.verbose:
                print(f"Warning: Could not generate bootloader: {e}")

        if self.verbose:
            print("[12/12] Generating partition table...")
        try:
            partitions_bin = linker.generate_partition_table()
        except KeyboardInterrupt as ke:
            from zapio.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            if self.verbose:
                print(f"Warning: Could not generate partition table: {e}")

        return bootloader_bin, partitions_bin

    def _print_success(
        self,
        build_time: float,
        firmware_elf: Path,
        firmware_bin: Path,
        bootloader_bin: Optional[Path],
        partitions_bin: Optional[Path]
    ) -> None:
        """
        Print build success message.

        Args:
            build_time: Total build time
            firmware_elf: Path to firmware ELF
            firmware_bin: Path to firmware binary
            bootloader_bin: Optional path to bootloader
            partitions_bin: Optional path to partition table
        """
        print()
        print("=" * 60)
        print("BUILD SUCCESSFUL!")
        print("=" * 60)
        print(f"  Build time: {build_time:.2f}s")
        print(f"  Firmware ELF: {firmware_elf}")
        print(f"  Firmware BIN: {firmware_bin}")
        if bootloader_bin:
            print(f"  Bootloader: {bootloader_bin}")
        if partitions_bin:
            print(f"  Partitions: {partitions_bin}")
        print()

    def _error_result(self, start_time: float, message: str) -> ESP32BuildResult:
        """
        Create an error result.

        Args:
            start_time: Build start time
            message: Error message

        Returns:
            ESP32BuildResult indicating failure
        """
        return ESP32BuildResult(
            success=False,
            firmware_bin=None,
            firmware_elf=None,
            bootloader_bin=None,
            partitions_bin=None,
            size_info=None,
            build_time=time.time() - start_time,
            message=message
        )
