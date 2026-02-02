"""
STM32-specific build orchestration for Fbuild projects.

This module handles STM32 platform builds separately from other platforms,
providing cleaner separation of concerns and better maintainability.
"""

import _thread
import logging
import time
from pathlib import Path
from typing import TYPE_CHECKING, List, Optional
from dataclasses import dataclass

if TYPE_CHECKING:
    from fbuild.daemon.compilation_queue import CompilationJobQueue

from .. import platform_configs
from ..packages import Cache
from ..packages.platform_stm32 import PlatformSTM32
from ..packages.toolchain_stm32 import ToolchainSTM32
from ..packages.library_manager import LibraryManager, LibraryError
from ..config.board_config import BoardConfig
from .configurable_compiler import ConfigurableCompiler
from .configurable_linker import ConfigurableLinker
from .linker import SizeInfo
from .orchestrator import IBuildOrchestrator, BuildResult, managed_compilation_queue
from .build_utils import safe_rmtree
from .build_state import BuildStateTracker
from .build_info_generator import BuildInfoGenerator
from ..subprocess_utils import safe_run

# Module-level logger
logger = logging.getLogger(__name__)


@dataclass
class BuildResultSTM32:
    """Result of an STM32 build operation (internal use)."""

    success: bool
    firmware_hex: Optional[Path]
    firmware_bin: Optional[Path]
    firmware_elf: Optional[Path]
    size_info: Optional[SizeInfo]
    build_time: float
    message: str


class OrchestratorSTM32(IBuildOrchestrator):
    """
    Orchestrates STM32-specific build process.

    Handles platform initialization, toolchain setup, framework preparation,
    and firmware generation for STM32 targets.
    """

    def __init__(self, cache: Cache, verbose: bool = False):
        """
        Initialize STM32 orchestrator.

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
        verbose: Optional[bool] = None,
        jobs: int | None = None,
        queue: Optional["CompilationJobQueue"] = None,
    ) -> BuildResult:
        """Execute complete build process (IBuildOrchestrator interface).

        Args:
            project_dir: Project root directory containing platformio.ini
            env_name: Environment name to build (defaults to first/default env)
            clean: Clean build (remove all artifacts before building)
            jobs: Number of parallel compilation jobs (None = CPU count, 1 = serial)
            verbose: Override verbose setting
            queue: Compilation queue from daemon context (injected by build_processor)

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
            board_id = env_config.get("board", "nucleo_f446re")
            build_flags = config.get_build_flags(env_name)
            lib_deps = config.get_lib_deps(env_name)

            # Call internal build method
            stm32_result = self._build_stm32(
                project_dir, env_name, board_id, env_config, build_flags, lib_deps, clean, verbose_mode, jobs, queue
            )

            # Convert BuildResultSTM32 to BuildResult
            return BuildResult(
                success=stm32_result.success,
                hex_path=stm32_result.firmware_hex,
                elf_path=stm32_result.firmware_elf,
                size_info=stm32_result.size_info,
                build_time=stm32_result.build_time,
                message=stm32_result.message
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

    def _build_stm32(
        self,
        project_dir: Path,
        env_name: str,
        board_id: str,
        env_config: dict,
        build_flags: List[str],
        lib_deps: List[str],
        clean: bool = False,
        verbose: bool = False,
        jobs: int | None = None,
        queue: Optional["CompilationJobQueue"] = None,
    ) -> BuildResultSTM32:
        """
        Execute complete STM32 build process (internal implementation).

        Args:
            project_dir: Project directory
            env_name: Environment name
            board_id: Board ID (e.g., nucleo_f446re, bluepill_f103c8)
            env_config: Environment configuration dict
            build_flags: User build flags from platformio.ini
            lib_deps: Library dependencies from platformio.ini
            clean: Whether to clean before build
            verbose: Verbose output mode
            jobs: Number of parallel compilation jobs (None = CPU count, 1 = serial)

        Returns:
            BuildResultSTM32 with build status and output paths
        """
        start_time = time.time()

        try:
            # Get board configuration
            from ..config.board_config import BoardConfig

            if verbose:
                logger.info("[2/7] Loading board configuration...")
            else:
                logger.info("Loading board configuration...")

            board_config = BoardConfig.from_board_id(board_id)

            # Initialize platform
            if verbose:
                logger.info("[3/7] Initializing STM32 platform...")
            else:
                logger.info("Initializing STM32 platform...")

            platform = PlatformSTM32(
                self.cache,
                board_config.mcu,
                show_progress=True
            )
            platform.ensure_package()

            if verbose:
                logger.info(f"      Board: {board_id}")
                logger.info(f"      MCU: {board_config.mcu}")
                logger.info(f"      CPU Frequency: {board_config.f_cpu}")

            # Setup build directory
            build_dir = self._setup_build_directory(env_name, clean, verbose)

            # Check build state and invalidate cache if needed
            if verbose:
                logger.info("[3.5/7] Checking build configuration state...")

            state_tracker = BuildStateTracker(build_dir)
            needs_rebuild, reasons, current_state = state_tracker.check_invalidation(
                platformio_ini_path=project_dir / "platformio.ini",
                platform="ststm32",
                board=board_id,
                framework=env_config.get('framework', 'arduino'),
                toolchain_version=platform.toolchain.version,
                framework_version=platform.framework.version,
                platform_version=f"stm32-{platform.framework.version}",
                build_flags=build_flags,
                lib_deps=lib_deps,
            )

            if needs_rebuild:
                if verbose:
                    logger.info("      Build cache invalidated:")
                    for reason in reasons:
                        logger.info(f"        - {reason}")
                    logger.info("      Cleaning build artifacts...")
                # Clean build artifacts to force rebuild
                if build_dir.exists():
                    safe_rmtree(build_dir)
                # Recreate build directory
                build_dir.mkdir(parents=True, exist_ok=True)
            else:
                if verbose:
                    logger.info("      Build configuration unchanged, using cached artifacts")

            # Load platform configuration from package data
            mcu_family = platform._get_mcu_family(board_config.mcu).lower().replace("xx", "")
            platform_config = platform_configs.load_config(mcu_family)

            # Initialize compiler
            if verbose:
                logger.info("[4/7] Compiling Arduino core...")
            else:
                logger.info("Compiling Arduino core...")

            # Use managed compilation queue context manager for safe resource handling
            with managed_compilation_queue(jobs, verbose, provided_queue=queue) as compilation_queue:
                compiler = ConfigurableCompiler(
                    platform,
                    platform.toolchain,
                    platform.framework,
                    board_id,
                    build_dir,
                    platform_config=platform_config,
                    show_progress=verbose,
                    user_build_flags=build_flags,
                    compilation_queue=compilation_queue,
                    cache=self.cache,
                )

                # Compile Arduino core with progress bar
                if verbose:
                    core_obj_files = compiler.compile_core()
                else:
                    # Use tqdm progress bar for non-verbose mode
                    from tqdm import tqdm

                    # Get number of core source files for progress tracking
                    core_sources = platform.framework.get_core_sources("arduino")
                    total_files = len(core_sources)

                    # Create progress bar
                    with tqdm(
                        total=total_files,
                        desc='Compiling Arduino core',
                        unit='file',
                        ncols=80,
                        leave=False
                    ) as pbar:
                        core_obj_files = compiler.compile_core(progress_bar=pbar)

                    # Print completion message
                    logger.info(f"Compiled {len(core_obj_files)} core files")

                core_archive = compiler.create_core_archive(core_obj_files)

                if verbose:
                    logger.info(f"      Compiled {len(core_obj_files)} core source files")

                # Handle library dependencies (if any)
                library_archives, library_include_paths = self._process_libraries(
                    env_config, build_dir, compiler, platform.toolchain, board_config, verbose, project_dir=project_dir
                )

                # Add library include paths to compiler
                if library_include_paths:
                    compiler.add_library_includes(library_include_paths)

                # Get src_dir override from platformio.ini
                from ..config import PlatformIOConfig
                config_for_src_dir = PlatformIOConfig(project_dir / "platformio.ini")
                src_dir_override = config_for_src_dir.get_src_dir()

                # Find and compile sketch
                sketch_obj_files = self._compile_sketch(project_dir, compiler, start_time, verbose, src_dir_override)
                if sketch_obj_files is None:
                    search_dir = project_dir / src_dir_override if src_dir_override else project_dir
                    return self._error_result(
                        start_time,
                        f"No .ino sketch file found in {search_dir}"
                    )

                # Initialize linker
                if verbose:
                    logger.info("[6/7] Linking firmware...")
                else:
                    logger.info("Linking firmware...")

                linker = ConfigurableLinker(
                    platform,
                    platform.toolchain,
                    platform.framework,
                    board_id,
                    build_dir,
                    platform_config=platform_config,
                    show_progress=verbose
                )

                # Link firmware
                firmware_elf = linker.link(sketch_obj_files, core_archive, library_archives=library_archives)

                # Generate bin and hex files
                if verbose:
                    logger.info("[7/7] Generating firmware...")
                else:
                    logger.info("Generating firmware...")

                firmware_bin = linker.generate_bin(firmware_elf)
                firmware_hex = self._generate_hex(firmware_elf, platform.toolchain, verbose)

                # Get size info
                size_info = linker.get_size_info(firmware_elf)

                build_time = time.time() - start_time

                if verbose:
                    self._print_success(
                        build_time, firmware_elf, firmware_hex, size_info
                    )

                # Save build state for future cache validation
                if verbose:
                    logger.info("[7.5/7] Saving build state...")
                state_tracker.save_state(current_state)

                # Generate build_info.json
                build_info_generator = BuildInfoGenerator(build_dir)
                # Parse f_cpu from string (e.g., "180000000L") to int
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
                    platform="ststm32",
                    f_cpu=f_cpu_int,
                    build_time=build_time,
                    elf_path=firmware_elf,
                    hex_path=firmware_hex,
                    bin_path=firmware_bin,
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
                    logger.info(f"      Build info saved to {build_info_generator.build_info_path}")

                return BuildResultSTM32(
                    success=True,
                    firmware_hex=firmware_hex,
                    firmware_bin=firmware_bin,
                    firmware_elf=firmware_elf,
                    size_info=size_info,
                    build_time=build_time,
                    message="Build successful (native STM32 build)"
                )

        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            build_time = time.time() - start_time
            import traceback
            error_trace = traceback.format_exc()
            return BuildResultSTM32(
                success=False,
                firmware_hex=None,
                firmware_bin=None,
                firmware_elf=None,
                size_info=None,
                build_time=build_time,
                message=f"STM32 native build failed: {e}\n\n{error_trace}"
            )

    def _generate_hex(self, elf_path: Path, toolchain: ToolchainSTM32, verbose: bool = False) -> Path:
        """Generate HEX file from ELF file.

        Args:
            elf_path: Path to input ELF file
            toolchain: STM32 toolchain instance
            verbose: Verbose output mode

        Returns:
            Path to generated HEX file

        Raises:
            Exception: If HEX generation fails
        """

        hex_path = elf_path.parent / f"{elf_path.stem}.hex"

        if verbose:
            logger.info(f"      Generating HEX file: {hex_path.name}")

        objcopy = toolchain.get_objcopy_path()
        if objcopy is None:
            raise Exception("objcopy not found in toolchain")

        cmd = [
            str(objcopy),
            "-O", "ihex",
            str(elf_path),
            str(hex_path)
        ]

        result = safe_run(cmd, capture_output=True, text=True)
        if result.returncode != 0:
            raise Exception(f"objcopy failed: {result.stderr}")

        if verbose:
            logger.info(f"      HEX file generated: {hex_path}")

        return hex_path

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
                logger.info("[1/7] Cleaning build directory...")
            else:
                logger.info("Cleaning build directory...")
            safe_rmtree(build_dir)

        build_dir.mkdir(parents=True, exist_ok=True)
        return build_dir

    def _process_libraries(
        self,
        env_config: dict,
        build_dir: Path,
        compiler: ConfigurableCompiler,
        toolchain: ToolchainSTM32,
        board_config: BoardConfig,
        verbose: bool,
        project_dir: Optional[Path] = None
    ) -> tuple[List[Path], List[Path]]:
        """
        Process and compile library dependencies.

        Args:
            env_config: Environment configuration
            build_dir: Build directory
            compiler: Configured compiler instance
            toolchain: STM32 toolchain instance
            board_config: Board configuration instance
            verbose: Verbose output mode
            project_dir: Optional project directory for resolving relative library paths

        Returns:
            Tuple of (library_archives, library_include_paths)
        """
        lib_deps = env_config.get('lib_deps', '')
        library_archives = []
        library_include_paths = []

        if not lib_deps:
            return library_archives, library_include_paths

        if verbose:
            logger.info("[4.5/7] Processing library dependencies...")

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
                logger.info(f"      Found {len(lib_specs)} library dependencies")

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
                logger.info(f"      Compiled {len(libraries)} libraries")

        except LibraryError as e:
            logger.warning(f"      Error processing libraries: {e}")
            # Continue build without libraries
            library_archives = []
            library_include_paths = []

        return library_archives, library_include_paths

    def _compile_sketch(
        self,
        project_dir: Path,
        compiler: ConfigurableCompiler,
        start_time: float,
        verbose: bool,
        src_dir_override: Optional[str] = None
    ) -> Optional[List[Path]]:
        """
        Find and compile sketch files.

        Args:
            project_dir: Project directory
            compiler: Configured compiler instance
            start_time: Build start time
            verbose: Verbose output mode
            src_dir_override: Optional src_dir override from platformio.ini

        Returns:
            List of compiled object files, or None if no sketch found
        """
        if verbose:
            logger.info("[5/7] Compiling sketch...")
        else:
            logger.info("Compiling sketch...")

        # Determine source directory
        if src_dir_override:
            src_dir = project_dir / src_dir_override
        else:
            src_dir = project_dir / "src"
            if not src_dir.exists():
                src_dir = project_dir

        # Find sketch file (.ino or .cpp)
        sketch_files = list(src_dir.glob("*.ino"))
        if not sketch_files:
            sketch_files = list(src_dir.glob("*.cpp"))

        if not sketch_files:
            return None

        # Also find additional source files
        cpp_files = list(src_dir.glob("*.cpp"))
        c_files = list(src_dir.glob("*.c"))
        all_source_files = sketch_files + [f for f in cpp_files if f not in sketch_files] + c_files

        if verbose:
            logger.info(f"      Found {len(all_source_files)} source files")

        # Compile sketch files - compile each file individually
        obj_files = []
        for source_file in all_source_files:
            if source_file.suffix == '.ino':
                # .ino files need preprocessing
                compiled = compiler.compile_sketch(source_file)
                obj_files.extend(compiled)
            else:
                # .c and .cpp files can be compiled directly
                obj_dir = compiler.build_dir / "obj"
                obj_dir.mkdir(parents=True, exist_ok=True)
                obj_path = obj_dir / f"{source_file.stem}.o"

                # Skip if up-to-date
                if not compiler.needs_rebuild(source_file, obj_path):
                    obj_files.append(obj_path)
                    continue

                compiled_obj = compiler.compile_source(source_file, obj_path)
                obj_files.append(compiled_obj)

        if verbose:
            logger.info(f"      Compiled {len(obj_files)} sketch files")

        return obj_files

    def _error_result(self, start_time: float, message: str) -> BuildResultSTM32:
        """Create an error result."""
        return BuildResultSTM32(
            success=False,
            firmware_hex=None,
            firmware_bin=None,
            firmware_elf=None,
            size_info=None,
            build_time=time.time() - start_time,
            message=message
        )

    def _print_success(
        self,
        build_time: float,
        firmware_elf: Path,
        firmware_hex: Path,
        size_info: Optional[SizeInfo]
    ) -> None:
        """Print success message with build details."""
        logger.info("")
        logger.info("=" * 60)
        logger.info("STM32 BUILD SUCCESSFUL")
        logger.info("=" * 60)
        logger.info(f"Build time: {build_time:.2f}s")
        logger.info(f"Output: {firmware_hex}")

        if size_info:
            logger.info(f"Flash: {size_info.total_flash:,} bytes")
            logger.info(f"RAM:   {size_info.total_ram:,} bytes")

        logger.info("=" * 60)
