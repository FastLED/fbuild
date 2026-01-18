"""
Teensy-specific build orchestration for Fbuild projects.

This module handles Teensy platform builds separately from AVR/ESP32 builds,
providing cleaner separation of concerns and better maintainability.
"""

import _thread
import time
from pathlib import Path
from typing import List, Optional
from dataclasses import dataclass

from ..packages import Cache
from ..packages.platform_teensy import PlatformTeensy
from ..packages.toolchain_teensy import ToolchainTeensy
from ..packages.library_manager import LibraryManager, LibraryError
from ..config.board_config import BoardConfig
from ..cli_utils import BannerFormatter
from .configurable_compiler import ConfigurableCompiler
from .configurable_linker import ConfigurableLinker
from .linker import SizeInfo
from .orchestrator import IBuildOrchestrator, BuildResult
from .build_utils import safe_rmtree
from .build_state import BuildStateTracker
from .build_info_generator import BuildInfoGenerator


@dataclass
class BuildResultTeensy:
    """Result of a Teensy build operation (internal use)."""

    success: bool
    firmware_hex: Optional[Path]
    firmware_elf: Optional[Path]
    size_info: Optional[SizeInfo]
    build_time: float
    message: str


class OrchestratorTeensy(IBuildOrchestrator):
    """
    Orchestrates Teensy-specific build process.

    Handles platform initialization, toolchain setup, framework preparation,
    and firmware generation for Teensy 4.x targets.
    """

    def __init__(self, cache: Cache, verbose: bool = False):
        """
        Initialize Teensy orchestrator.

        Args:
            cache: Cache instance for package management
            verbose: Enable verbose output
        """
        self.cache = cache
        self.verbose = verbose

    def build(
        self,
        project_dir: Path,
        env_name: Optional[str] = None,
        clean: bool = False,
        verbose: Optional[bool] = None
    ) -> BuildResult:
        """Execute complete build process (IBuildOrchestrator interface).

        Args:
            project_dir: Project root directory containing platformio.ini
            env_name: Environment name to build (defaults to first/default env)
            clean: Clean build (remove all artifacts before building)
            verbose: Override verbose setting

        Returns:
            BuildResult with build status and output paths

        Raises:
            BuildOrchestratorError: If build fails at any phase
        """
        from ..config import PlatformIOConfig

        verbose_mode = verbose if verbose is not None else self.verbose

        # Parse platformio.ini to get environment configuration
        ini_path = project_dir / "platformio.ini"
        if not ini_path.exists():
            return BuildResult(
                success=False,
                hex_path=None,
                elf_path=None,
                size_info=None,
                build_time=0.0,
                message=f"platformio.ini not found in {project_dir}"
            )

        try:
            config = PlatformIOConfig(ini_path)

            # Determine environment to build
            if env_name is None:
                env_name = config.get_default_environment()
                if env_name is None:
                    return BuildResult(
                        success=False,
                        hex_path=None,
                        elf_path=None,
                        size_info=None,
                        build_time=0.0,
                        message="No environment specified and no default found in platformio.ini"
                    )

            env_config = config.get_env_config(env_name)
            board_id = env_config.get("board", "teensy41")
            build_flags = config.get_build_flags(env_name)
            lib_deps = config.get_lib_deps(env_name)

            # Call internal build method
            teensy_result = self._build_teensy(
                project_dir, env_name, board_id, env_config, build_flags, lib_deps, clean, verbose_mode
            )

            # Convert BuildResultTeensy to BuildResult
            return BuildResult(
                success=teensy_result.success,
                hex_path=teensy_result.firmware_hex,
                elf_path=teensy_result.firmware_elf,
                size_info=teensy_result.size_info,
                build_time=teensy_result.build_time,
                message=teensy_result.message
            )

        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception as e:
            return BuildResult(
                success=False,
                hex_path=None,
                elf_path=None,
                size_info=None,
                build_time=0.0,
                message=f"Failed to parse configuration: {e}"
            )

    def _build_teensy(
        self,
        project_dir: Path,
        env_name: str,
        board_id: str,
        env_config: dict,
        build_flags: List[str],
        lib_deps: List[str],
        clean: bool = False,
        verbose: bool = False
    ) -> BuildResultTeensy:
        """
        Execute complete Teensy build process (internal implementation).

        Args:
            project_dir: Project directory
            env_name: Environment name
            board_id: Board ID (e.g., teensy41)
            env_config: Environment configuration dict
            build_flags: User build flags from platformio.ini
            lib_deps: Library dependencies from platformio.ini
            clean: Whether to clean before build
            verbose: Verbose output mode

        Returns:
            BuildResultTeensy with build status and output paths
        """
        start_time = time.time()

        try:
            # Get board configuration
            from ..config.board_config import BoardConfig

            if verbose:
                print("[2/7] Loading board configuration...")

            board_config = BoardConfig.from_board_id(board_id)

            # Initialize platform
            if verbose:
                print("[3/7] Initializing Teensy platform...")

            platform = PlatformTeensy(
                self.cache,
                board_config.mcu,
                show_progress=verbose
            )
            platform.ensure_package()

            if verbose:
                print(f"      Board: {board_id}")
                print(f"      MCU: {board_config.mcu}")
                print(f"      CPU Frequency: {board_config.f_cpu}")

            # Setup build directory
            build_dir = self._setup_build_directory(env_name, clean, verbose)

            # Check build state and invalidate cache if needed
            if verbose:
                print("[3.5/7] Checking build configuration state...")

            state_tracker = BuildStateTracker(build_dir)
            needs_rebuild, reasons, current_state = state_tracker.check_invalidation(
                platformio_ini_path=project_dir / "platformio.ini",
                platform="teensy",
                board=board_id,
                framework=env_config.get('framework', 'arduino'),
                toolchain_version=platform.toolchain.version,
                framework_version=platform.framework.version,
                platform_version=f"teensy-{platform.framework.version}",
                build_flags=build_flags,
                lib_deps=lib_deps,
            )

            if needs_rebuild:
                if verbose:
                    print("      Build cache invalidated:")
                    for reason in reasons:
                        print(f"        - {reason}")
                    print("      Cleaning build artifacts...")
                # Clean build artifacts to force rebuild
                if build_dir.exists():
                    safe_rmtree(build_dir)
                # Recreate build directory
                build_dir.mkdir(parents=True, exist_ok=True)
            else:
                if verbose:
                    print("      Build configuration unchanged, using cached artifacts")

            # Initialize compiler
            if verbose:
                print("[4/7] Compiling Arduino core...")

            compiler = ConfigurableCompiler(
                platform,
                platform.toolchain,
                platform.framework,
                board_id,
                build_dir,
                platform_config=None,
                show_progress=verbose,
                user_build_flags=build_flags
            )

            # Compile Arduino core
            core_obj_files = compiler.compile_core()
            core_archive = compiler.create_core_archive(core_obj_files)

            if verbose:
                print(f"      Compiled {len(core_obj_files)} core source files")

            # Handle library dependencies (if any)
            library_archives, library_include_paths = self._process_libraries(
                env_config, build_dir, compiler, platform.toolchain, board_config, verbose
            )

            # Add library include paths to compiler
            if library_include_paths:
                compiler.add_library_includes(library_include_paths)

            # Find and compile sketch
            sketch_obj_files = self._compile_sketch(project_dir, compiler, start_time, verbose)
            if sketch_obj_files is None:
                return self._error_result(
                    start_time,
                    f"No .ino sketch file found in {project_dir}"
                )

            # Initialize linker
            if verbose:
                print("[6/7] Linking firmware...")

            linker = ConfigurableLinker(
                platform,
                platform.toolchain,
                platform.framework,
                board_id,
                build_dir,
                platform_config=None,
                show_progress=verbose
            )

            # Link firmware
            firmware_elf = linker.link(sketch_obj_files, core_archive, library_archives=library_archives)

            # Generate hex file
            if verbose:
                print("[7/7] Generating firmware hex...")

            firmware_hex = linker.generate_hex(firmware_elf)

            # Get size info
            size_info = linker.get_size_info(firmware_elf)

            build_time = time.time() - start_time

            if verbose:
                self._print_success(
                    build_time, firmware_elf, firmware_hex, size_info
                )

            # Save build state for future cache validation
            if verbose:
                print("[7.5/7] Saving build state...")
            state_tracker.save_state(current_state)

            # Generate build_info.json
            build_info_generator = BuildInfoGenerator(build_dir)
            # Parse f_cpu from string (e.g., "600000000L") to int
            f_cpu_int = int(board_config.f_cpu.rstrip("L"))
            # Build toolchain_paths dict, filtering out None values
            toolchain_paths_raw = {
                "gcc": platform.toolchain.get_gcc_path(),
                "gxx": platform.toolchain.get_gxx_path(),
                "ar": platform.toolchain.get_ar_path(),
                "objcopy": platform.toolchain.get_objcopy_path(),
                "size": platform.toolchain.get_size_path(),
            }
            toolchain_paths = {k: v for k, v in toolchain_paths_raw.items() if v is not None}
            build_info = build_info_generator.generate_generic(
                env_name=env_name,
                board_id=board_id,
                board_name=board_config.name,
                mcu=board_config.mcu,
                platform="teensy",
                f_cpu=f_cpu_int,
                build_time=build_time,
                elf_path=firmware_elf,
                hex_path=firmware_hex,
                size_info=size_info,
                build_flags=build_flags,
                lib_deps=lib_deps,
                toolchain_version=platform.toolchain.version,
                toolchain_paths=toolchain_paths,
                framework_name="arduino",
                framework_version=platform.framework.version,
                core_path=platform.framework.get_cores_dir(),
            )
            build_info_generator.save(build_info)
            if verbose:
                print(f"      Build info saved to {build_info_generator.build_info_path}")

            return BuildResultTeensy(
                success=True,
                firmware_hex=firmware_hex,
                firmware_elf=firmware_elf,
                size_info=size_info,
                build_time=build_time,
                message="Build successful (native Teensy build)"
            )

        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            build_time = time.time() - start_time
            import traceback
            error_trace = traceback.format_exc()
            return BuildResultTeensy(
                success=False,
                firmware_hex=None,
                firmware_elf=None,
                size_info=None,
                build_time=build_time,
                message=f"Teensy native build failed: {e}\n\n{error_trace}"
            )

    def _setup_build_directory(self, env_name: str, clean: bool, verbose: bool) -> Path:
        """
        Setup build directory with optional cleaning.

        Args:
            env_name: Environment name
            clean: Whether to clean before build
            verbose: Verbose output mode

        Returns:
            Build directory path
        """
        build_dir = self.cache.get_build_dir(env_name)

        if clean and build_dir.exists():
            if verbose:
                print("[1/7] Cleaning build directory...")
            safe_rmtree(build_dir)

        build_dir.mkdir(parents=True, exist_ok=True)
        return build_dir

    def _process_libraries(
        self,
        env_config: dict,
        build_dir: Path,
        compiler: ConfigurableCompiler,
        toolchain: ToolchainTeensy,
        board_config: BoardConfig,
        verbose: bool
    ) -> tuple[List[Path], List[Path]]:
        """
        Process and compile library dependencies.

        Args:
            env_config: Environment configuration
            build_dir: Build directory
            compiler: Configured compiler instance
            toolchain: Teensy toolchain instance
            board_config: Board configuration instance
            verbose: Verbose output mode

        Returns:
            Tuple of (library_archives, library_include_paths)
        """
        lib_deps = env_config.get('lib_deps', '')
        library_archives = []
        library_include_paths = []

        if not lib_deps:
            return library_archives, library_include_paths

        if verbose:
            print("[4.5/7] Processing library dependencies...")

        # Parse lib_deps (can be string or list)
        if isinstance(lib_deps, str):
            lib_specs = [dep.strip() for dep in lib_deps.split('\n') if dep.strip()]
        else:
            lib_specs = lib_deps

        if not lib_specs:
            return library_archives, library_include_paths

        try:
            # Initialize library manager
            library_manager = LibraryManager(build_dir, mode="release")

            # Prepare compilation parameters
            lib_defines = []
            defines_dict = board_config.get_defines()
            for key, value in defines_dict.items():
                if value:
                    lib_defines.append(f"{key}={value}")
                else:
                    lib_defines.append(key)

            # Get include paths from compiler configuration
            lib_includes = compiler.get_include_paths()

            # Get compiler path from toolchain (use C++ compiler for libraries)
            compiler_path = toolchain.get_gxx_path()
            if compiler_path is None:
                raise LibraryError("C++ compiler not found in toolchain")

            if verbose:
                print(f"      Found {len(lib_specs)} library dependencies")
                print(f"      Compiler path: {compiler_path}")

            # Ensure all libraries are downloaded and compiled
            libraries = library_manager.ensure_libraries(
                lib_deps=lib_specs,
                compiler_path=compiler_path,
                mcu=board_config.mcu,
                f_cpu=board_config.f_cpu,
                defines=lib_defines,
                include_paths=lib_includes,
                extra_flags=[],
                show_progress=verbose
            )

            # Get library artifacts
            library_include_paths = library_manager.get_library_include_paths()
            library_archives = library_manager.get_library_objects()

            if verbose:
                print(f"      Compiled {len(libraries)} libraries")
                print(f"      Library objects: {len(library_archives)}")

        except LibraryError as e:
            print(f"      Error processing libraries: {e}")
            # Continue build without libraries
            library_archives = []
            library_include_paths = []

        return library_archives, library_include_paths

    def _compile_sketch(
        self,
        project_dir: Path,
        compiler: ConfigurableCompiler,
        start_time: float,
        verbose: bool
    ) -> Optional[List[Path]]:
        """
        Find and compile sketch files.

        Args:
            project_dir: Project directory
            compiler: Configured compiler instance
            start_time: Build start time for error reporting
            verbose: Verbose output mode

        Returns:
            List of compiled object files or None if no sketch found
        """
        if verbose:
            print("[5/7] Compiling sketch...")

        # Look for .ino files in the project directory
        sketch_files = list(project_dir.glob("*.ino"))
        if not sketch_files:
            # Also check src/ directory
            src_dir = project_dir / "src"
            if src_dir.exists():
                sketch_files = list(src_dir.glob("*.ino"))

        if not sketch_files:
            return None

        sketch_path = sketch_files[0]
        sketch_obj_files = compiler.compile_sketch(sketch_path)

        if verbose:
            print(f"      Compiled {len(sketch_obj_files)} sketch file(s)")

        return sketch_obj_files

    def _print_success(
        self,
        build_time: float,
        firmware_elf: Path,
        firmware_hex: Path,
        size_info: Optional[SizeInfo]
    ) -> None:
        """
        Print build success message.

        Args:
            build_time: Total build time
            firmware_elf: Path to firmware ELF
            firmware_hex: Path to firmware hex
            size_info: Size information
        """
        # Build success message
        message_lines = ["BUILD SUCCESSFUL!"]
        message_lines.append(f"Build time: {build_time:.2f}s")
        message_lines.append(f"Firmware ELF: {firmware_elf}")
        message_lines.append(f"Firmware HEX: {firmware_hex}")

        if size_info:
            message_lines.append(
                f"Program size: {size_info.text + size_info.data} bytes"
            )
            message_lines.append(f"Data size: {size_info.bss + size_info.data} bytes")

        BannerFormatter.print_banner("\n".join(message_lines), width=60, center=False)

    def _error_result(self, start_time: float, message: str) -> BuildResultTeensy:
        """
        Create an error result.

        Args:
            start_time: Build start time
            message: Error message

        Returns:
            BuildResultTeensy indicating failure
        """
        return BuildResultTeensy(
            success=False,
            firmware_hex=None,
            firmware_elf=None,
            size_info=None,
            build_time=time.time() - start_time,
            message=message
        )
