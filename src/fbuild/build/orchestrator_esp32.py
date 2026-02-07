"""
ESP32-specific build orchestration for Fbuild projects.

This module handles ESP32 platform builds separately from AVR builds,
providing cleaner separation of concerns and better maintainability.
"""

import _thread
import logging
import time
from pathlib import Path
from typing import TYPE_CHECKING, List, Optional
from dataclasses import dataclass

if TYPE_CHECKING:
    from .build_context import BuildParams

from ..packages import Cache
from ..packages.platform_esp32 import PlatformESP32
from ..packages.toolchain_esp32 import ToolchainESP32
from ..packages.framework_esp32 import FrameworkESP32
from ..packages.library_manager_esp32 import LibraryManagerESP32
from ..cli_utils import BannerFormatter
from .configurable_compiler import ConfigurableCompiler
from .configurable_linker import ConfigurableLinker
from .linker import SizeInfo
from .orchestrator import IBuildOrchestrator, BuildResult
from .build_state import BuildStateTracker
from .build_info_generator import BuildInfoGenerator
from ..output import log_phase, log_detail, log_warning, DefaultProgressCallback
from .psram_utils import board_has_psram, get_psram_mode

# Module-level logger
logger = logging.getLogger(__name__)


@dataclass
class BuildResultESP32:
    """Result of an ESP32 build operation (internal use)."""

    success: bool
    firmware_bin: Optional[Path]
    firmware_elf: Optional[Path]
    bootloader_bin: Optional[Path]
    partitions_bin: Optional[Path]
    merged_bin: Optional[Path]
    size_info: Optional[SizeInfo]
    build_time: float
    message: str


class OrchestratorESP32(IBuildOrchestrator):
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

    @staticmethod
    def board_has_psram(board_id: str) -> bool:
        """Delegate to module-level function. See psram_utils.board_has_psram."""
        return board_has_psram(board_id)

    @staticmethod
    def get_psram_mode(board_id: str, board_config: dict) -> str:
        """Delegate to module-level function. See psram_utils.get_psram_mode."""
        return get_psram_mode(board_id, board_config)

    def _add_psram_flags(self, board_id: str, mcu: str, build_flags: List[str], board_json: dict, verbose: bool) -> List[str]:
        """
        Add PSRAM-specific build flags based on board capabilities.

        IMPORTANT: We do NOT automatically add -DBOARD_HAS_PSRAM based on heuristics.
        PlatformIO's approach is that boards WITH PSRAM have -DBOARD_HAS_PSRAM in their
        board JSON's extra_flags. We trust the board JSON and only add supplementary
        flags like CONFIG_SPIRAM_USE_MALLOC if BOARD_HAS_PSRAM is already present.

        For ESP32-S3 boards WITHOUT -DBOARD_HAS_PSRAM, we add CONFIG_ESP32S3_DATA_CACHE_64KB
        to prevent "CORRUPT HEAP" crashes.

        Args:
            board_id: Board identifier (e.g., "seeed_xiao_esp32s3")
            mcu: MCU type (e.g., "esp32s3")
            build_flags: Existing build flags from platformio.ini
            board_json: Board configuration from platform board JSON file
            verbose: Enable verbose logging

        Returns:
            Modified build flags list with PSRAM flags added
        """
        # Create a new list to avoid modifying the original
        flags = build_flags.copy()

        # Only apply PSRAM handling to ESP32-S3 (other ESP32 variants handle PSRAM differently)
        if mcu != "esp32s3":
            return flags

        # Check if the board JSON's extra_flags contain -DBOARD_HAS_PSRAM
        # This is the authoritative source - we don't guess based on board name
        arduino_extra_flags = board_json.get("build", {}).get("extra_flags", [])
        if isinstance(arduino_extra_flags, str):
            arduino_extra_flags = arduino_extra_flags.split()

        # Also check build.arduino.extra_flags (some boards use this nested structure)
        arduino_config_extra_flags = board_json.get("build", {}).get("arduino", {}).get("extra_flags", [])
        if isinstance(arduino_config_extra_flags, str):
            arduino_config_extra_flags = arduino_config_extra_flags.split()

        # Combine all extra_flags sources
        all_extra_flags = arduino_extra_flags + arduino_config_extra_flags + flags

        has_psram_flag = "-DBOARD_HAS_PSRAM" in all_extra_flags

        if has_psram_flag:
            # Board JSON declares PSRAM - add supplementary PSRAM flags
            log_detail("Board has -DBOARD_HAS_PSRAM in extra_flags", verbose_only=verbose)
            if "-DCONFIG_SPIRAM_USE_MALLOC" not in flags:
                flags.append("-DCONFIG_SPIRAM_USE_MALLOC")
                log_detail("Adding PSRAM malloc flag: -DCONFIG_SPIRAM_USE_MALLOC", verbose_only=verbose)
        else:
            # Board JSON does NOT declare PSRAM - add cache config flag for heap stability
            log_detail(f"Board {board_id} has no -DBOARD_HAS_PSRAM in extra_flags (no PSRAM)", verbose_only=verbose)
            if "-DCONFIG_ESP32S3_DATA_CACHE_64KB" not in flags:
                flags.append("-DCONFIG_ESP32S3_DATA_CACHE_64KB")
                log_detail("Adding cache config flag for no-PSRAM board: -DCONFIG_ESP32S3_DATA_CACHE_64KB", verbose_only=verbose)

        return flags

    def build(self, request: "BuildParams") -> BuildResult:
        """Execute complete build process.

        Args:
            request: Build request with basic parameters from build_processor

        Returns:
            BuildResult with build status and output paths

        Raises:
            BuildOrchestratorError: If build fails at any phase
        """
        from ..config import PlatformIOConfig

        # Extract from request
        project_dir = request.project_dir
        env_name = request.env_name

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

            env_config = config.get_env_config(env_name)
            board_id = env_config.get("board", "")
            build_flags = config.get_build_flags(env_name)

            # Add debug logging for lib_deps
            logger.debug(f"[ORCHESTRATOR] About to call config.get_lib_deps('{env_name}')")
            lib_deps = config.get_lib_deps(env_name)
            logger.debug(f"[ORCHESTRATOR] get_lib_deps returned: {lib_deps}")

            # Call internal build method
            esp32_result = self._build_esp32(
                board_id, env_config, build_flags, lib_deps, request
            )

            # Convert BuildResultESP32 to BuildResult
            return BuildResult(
                success=esp32_result.success,
                hex_path=esp32_result.firmware_bin,
                elf_path=esp32_result.firmware_elf,
                size_info=esp32_result.size_info,
                build_time=esp32_result.build_time,
                message=esp32_result.message
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

    def _build_esp32(
        self,
        board_id: str,
        env_config: dict,
        build_flags: List[str],
        lib_deps: List[str],
        request: "BuildParams",
    ) -> BuildResultESP32:
        """
        Execute complete ESP32 build process (internal implementation).

        Args:
            board_id: Board ID (e.g., esp32-c6-devkitm-1)
            env_config: Environment configuration dict
            build_flags: User build flags from platformio.ini
            lib_deps: Library dependencies from platformio.ini
            request: Build request with basic parameters

        Returns:
            BuildResultESP32 with build status and output paths
        """
        start_time = time.time()

        # Extract from request
        project_dir = request.project_dir
        env_name = request.env_name
        verbose = request.verbose
        build_dir = request.build_dir

        try:
            # Get platform URL from env_config
            platform_url = env_config.get('platform')
            if not platform_url:
                return self._error_result(
                    start_time,
                    "No platform URL specified in platformio.ini"
                )

            # Resolve platform shorthand to actual download URL
            # PlatformIO supports formats like "platformio/espressif32" which need
            # to be converted to a real download URL
            platform_url = self._resolve_platform_url(platform_url)

            # Print build profile banner
            from .build_profiles import print_profile_banner
            print_profile_banner(request.profile)

            # Initialize platform
            log_phase(3, 13, "Initializing ESP32 platform...")

            platform = PlatformESP32(self.cache, platform_url, show_progress=True)
            platform.ensure_platform()

            # Get board configuration
            board_json = platform.get_board_json(board_id)
            mcu = board_json.get("build", {}).get("mcu", "esp32c6")

            log_detail(f"Board: {board_id}", verbose_only=True)
            log_detail(f"MCU: {mcu}", verbose_only=True)

            # Add PSRAM-specific build flags based on board capabilities
            # This prevents "CORRUPT HEAP" crashes on boards without PSRAM
            build_flags = self._add_psram_flags(board_id, mcu, build_flags, board_json, verbose)

            # Get required packages
            packages = platform.get_required_packages(mcu)

            # Initialize toolchain (pass MCU for Xtensa wrapper binary selection)
            toolchain = self._setup_toolchain(packages, start_time, verbose, mcu=mcu)
            if toolchain is None:
                return self._error_result(
                    start_time,
                    "Failed to initialize toolchain"
                )

            # Initialize framework
            framework = self._setup_framework(packages, start_time, verbose)
            if framework is None:
                return self._error_result(
                    start_time,
                    "Failed to initialize framework"
                )

            # Ensure build directory exists
            build_dir.mkdir(parents=True, exist_ok=True)

            # Determine source directory for cache invalidation
            # This is computed early to include source file changes in cache key
            from ..config import PlatformIOConfig
            config_for_src_dir = PlatformIOConfig(project_dir / "platformio.ini")
            src_dir_override = config_for_src_dir.get_src_dir()
            source_dir = project_dir / src_dir_override if src_dir_override else project_dir

            # Check build state and invalidate cache if needed
            log_detail("Checking build configuration state...", verbose_only=True)

            state_tracker = BuildStateTracker(build_dir)
            needs_rebuild, reasons, current_state = state_tracker.check_invalidation(
                platformio_ini_path=project_dir / "platformio.ini",
                platform="esp32",
                board=board_id,
                framework=env_config.get('framework', 'arduino'),
                toolchain_version=toolchain.version,
                framework_version=framework.version,
                platform_version=platform.version,
                build_flags=build_flags,
                lib_deps=lib_deps,
                source_dir=source_dir,
            )

            if needs_rebuild:
                log_detail("Build cache invalidated:", verbose_only=True)
                for reason in reasons:
                    log_detail(f"  - {reason}", indent=8, verbose_only=True)
                log_detail("Cleaning build artifacts...", verbose_only=True)
                # Clean build artifacts to force rebuild
                from .build_utils import safe_rmtree
                if build_dir.exists():
                    safe_rmtree(build_dir)
                # Recreate build directory
                build_dir.mkdir(parents=True, exist_ok=True)
            else:
                log_detail("Build configuration unchanged, using cached artifacts", verbose_only=True)

            # Initialize compilation executor early to show sccache status
            from .compilation_executor import CompilationExecutor
            compilation_executor = CompilationExecutor(
                build_dir=build_dir,
                show_progress=verbose,
                cache=self.cache,
                mcu=mcu,
                framework_version=framework.version,
            )

            # Load platform configuration ONCE (not redundantly in compiler/linker)
            from .. import platform_configs
            platform_config = platform_configs.load_config(mcu)
            if platform_config is None:
                return self._error_result(
                    start_time,
                    f"No platform configuration found for {mcu}. Available: {platform_configs.list_available_configs()}"
                )

            # Extract variant and core from board config
            variant = board_json.get("build", {}).get("variant", "")
            core = board_json.get("build", {}).get("core", "arduino")

            # Create full BuildContext with all configuration loaded once
            from .build_context import BuildContext
            context = BuildContext.from_request(
                request=request,
                platform=platform,
                toolchain=toolchain,
                mcu=mcu,
                framework_version=framework.version,
                compilation_executor=compilation_executor,
                cache=self.cache,
                # New consolidated fields
                framework=framework,
                board_id=board_id,
                board_config=board_json,
                platform_config=platform_config,
                variant=variant,
                core=core,
                user_build_flags=build_flags,
                env_config=env_config,
            )

            # Initialize compiler (uses BuildContext for all configuration)
            log_phase(7, 13, "Compiling Arduino core...")

            compiler = ConfigurableCompiler(context)

            # Create progress callback for detailed file-by-file tracking
            progress_callback = DefaultProgressCallback(verbose_only=not verbose)

            # Compile Arduino core with progress bar
            if verbose:
                core_obj_files = compiler.compile_core(progress_callback=progress_callback)
            else:
                # Use tqdm progress bar for non-verbose mode
                from tqdm import tqdm

                # Get number of core source files for progress tracking
                core_sources = framework.get_core_sources(compiler.core)
                total_files = len(core_sources)

                # Create progress bar
                with tqdm(
                    total=total_files,
                    desc='Compiling Arduino core',
                    unit='file',
                    ncols=80,
                    leave=False
                ) as pbar:
                    core_obj_files = compiler.compile_core(progress_bar=pbar, progress_callback=progress_callback)

                # Print completion message
                log_detail(f"Compiled {len(core_obj_files)} core files")

            # Add Bluetooth stub for non-ESP32 targets (ESP32-C6, ESP32-S3, etc.)
            # where esp32-hal-bt.c fails to compile but btInUse() is still referenced
            bt_stub_obj = self._create_bt_stub(build_dir, compiler, verbose)
            if bt_stub_obj:
                core_obj_files.append(bt_stub_obj)

            # Wait for all pending async compilation jobs (including bt_stub) to complete
            if hasattr(compiler, "wait_all_jobs"):
                compiler.wait_all_jobs()

            core_archive = compiler.create_core_archive(core_obj_files)

            log_detail(f"Compiled {len(core_obj_files)} core source files", verbose_only=True)

            # Handle library dependencies
            library_archives, library_include_paths = self._process_libraries(
                env_config, build_dir, compiler, toolchain, verbose, project_dir=project_dir
            )

            # Add library include paths to compiler
            if library_include_paths:
                compiler.add_library_includes(library_include_paths)

            # src_dir_override was computed earlier for cache invalidation

            # Find and compile sketch
            sketch_obj_files = self._compile_sketch(project_dir, compiler, start_time, verbose, src_dir_override)
            if sketch_obj_files is None:
                search_dir = project_dir / src_dir_override if src_dir_override else project_dir
                return self._error_result(
                    start_time,
                    f"No .ino sketch file found in {search_dir}"
                )

            # Initialize linker
            log_phase(10, 13, "Linking firmware...")

            logging.debug(f"orchestrator: env_config keys: {list(env_config.keys())}")
            logging.debug(f"orchestrator: board_build.partitions = {env_config.get('board_build.partitions', 'NOT FOUND')}")

            linker = ConfigurableLinker(context)

            # Link firmware
            firmware_elf = linker.link(sketch_obj_files, core_archive, library_archives=library_archives)

            # Generate binary
            log_phase(11, 13, "Generating firmware binary...")

            firmware_bin = linker.generate_bin(firmware_elf)

            # Generate bootloader and partition table
            bootloader_bin, partitions_bin = self._generate_boot_components(
                linker, mcu, verbose
            )

            # Generate merged bin if all components are available
            merged_bin = None
            if bootloader_bin and partitions_bin and firmware_bin:
                try:
                    merged_bin = linker.generate_merged_bin()
                except KeyboardInterrupt as ke:
                    from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
                    handle_keyboard_interrupt_properly(ke)
                    raise  # Never reached, but satisfies type checker
                except Exception as e:
                    log_warning(f"Could not generate merged bin: {e}")

            # Get size information from ELF file
            size_info = linker.get_size_info(firmware_elf)

            build_time = time.time() - start_time

            if verbose:
                self._print_success(
                    build_time, firmware_elf, firmware_bin,
                    bootloader_bin, partitions_bin, merged_bin, size_info
                )

            # Save build state for future cache validation
            log_detail("Saving build state...", verbose_only=True)
            state_tracker.save_state(current_state)

            # Generate build_info.json
            build_info_generator = BuildInfoGenerator(build_dir)
            board_name = board_json.get("name", board_id)
            # Parse f_cpu from string (e.g., "160000000L" or "160000000") to int
            f_cpu_raw = board_json.get("build", {}).get("f_cpu", "0")
            f_cpu_int = int(str(f_cpu_raw).rstrip("L")) if f_cpu_raw else 0
            # Build toolchain_paths dict, filtering out None values
            toolchain_paths_raw = {
                "gcc": toolchain.get_gcc_path(),
                "gxx": toolchain.get_gxx_path(),
                "ar": toolchain.get_ar_path(),
                "objcopy": toolchain.get_objcopy_path(),
                "size": toolchain.get_size_path(),
            }
            toolchain_paths = {k: v for k, v in toolchain_paths_raw.items() if v is not None}
            # Fallback flash settings from board JSON if not in env_config
            flash_mode_env = env_config.get("board_build.flash_mode")
            flash_mode_board = board_json.get("build", {}).get("flash_mode", "dio")
            flash_mode = flash_mode_env or flash_mode_board
            flash_size_env = env_config.get("board_build.flash_size")
            flash_size_board = board_json.get("upload", {}).get("flash_size", "4MB")
            flash_size = flash_size_env or flash_size_board
            print(f"[ORCHESTRATOR] FLASH_MODE: env={flash_mode_env}, board={flash_mode_board}, final={flash_mode}", flush=True)
            print(f"[ORCHESTRATOR] FLASH_SIZE: env={flash_size_env}, board={flash_size_board}, final={flash_size}", flush=True)
            logging.debug(f"FLASH_MODE: env={flash_mode_env}, board={flash_mode_board}, final={flash_mode}")
            logging.debug(f"FLASH_SIZE: env={flash_size_env}, board={flash_size_board}, final={flash_size}")
            build_info = build_info_generator.generate_esp32(
                env_name=env_name,
                board_id=board_id,
                board_name=board_name,
                mcu=mcu,
                f_cpu=f_cpu_int,
                build_time=build_time,
                elf_path=firmware_elf,
                bin_path=firmware_bin,
                size_info=size_info,
                build_flags=build_flags,
                lib_deps=lib_deps,
                toolchain_version=toolchain.version,
                toolchain_paths=toolchain_paths,
                framework_version=framework.version,
                core_path=framework.get_cores_dir(),
                bootloader_path=bootloader_bin,
                partitions_path=partitions_bin,
                application_offset=board_json.get("build", {}).get("app_offset", "0x10000"),
                flash_mode=flash_mode,
                flash_size=flash_size,
            )
            build_info_generator.save(build_info)
            log_detail(f"Build info saved to {build_info_generator.build_info_path}", verbose_only=True)

            return BuildResultESP32(
                success=True,
                firmware_bin=firmware_bin,
                firmware_elf=firmware_elf,
                bootloader_bin=bootloader_bin,
                partitions_bin=partitions_bin,
                merged_bin=merged_bin,
                size_info=size_info,
                build_time=build_time,
                message="Build successful (native ESP32 build)"
            )

        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            build_time = time.time() - start_time
            import traceback
            error_trace = traceback.format_exc()
            return BuildResultESP32(
                success=False,
                firmware_bin=None,
                firmware_elf=None,
                bootloader_bin=None,
                partitions_bin=None,
                merged_bin=None,
                size_info=None,
                build_time=build_time,
                message=f"ESP32 native build failed: {e}\n\n{error_trace}"
            )

    def _setup_toolchain(
        self,
        packages: dict,
        start_time: float,
        verbose: bool,
        mcu: Optional[str] = None,
    ) -> Optional['ToolchainESP32']:
        """
        Initialize ESP32 toolchain.

        Args:
            packages: Package URLs dictionary
            start_time: Build start time for error reporting
            verbose: Verbose output mode
            mcu: Target MCU for selecting MCU-specific wrapper binaries

        Returns:
            ToolchainESP32 instance or None on failure
        """
        log_phase(4, 13, "Initializing ESP32 toolchain...")

        toolchain_url = packages.get("toolchain-riscv32-esp") or packages.get("toolchain-xtensa-esp-elf")
        if not toolchain_url:
            return None

        # Determine toolchain type
        toolchain_type = "riscv32-esp" if "riscv32" in toolchain_url else "xtensa-esp-elf"
        toolchain = ToolchainESP32(
            self.cache,
            toolchain_url,
            toolchain_type,
            show_progress=True,
            mcu=mcu,
        )
        toolchain.ensure_toolchain()
        return toolchain

    def _setup_framework(
        self,
        packages: dict,
        start_time: float,
        verbose: bool
    ) -> Optional[FrameworkESP32]:
        """
        Initialize ESP32 framework.

        Args:
            packages: Package URLs dictionary
            start_time: Build start time for error reporting
            verbose: Verbose output mode

        Returns:
            FrameworkESP32 instance or None on failure
        """
        log_phase(5, 13, "Initializing ESP32 framework...")

        framework_url = packages.get("framework-arduinoespressif32")
        libs_url = packages.get("framework-arduinoespressif32-libs", "")

        if not framework_url:
            return None

        # Find skeleton library if present (e.g., framework-arduino-esp32c2-skeleton-lib)
        skeleton_lib_url = None
        for package_name, package_url in packages.items():
            if package_name.startswith("framework-arduino-") and package_name.endswith("-skeleton-lib"):
                skeleton_lib_url = package_url
                break

        framework = FrameworkESP32(
            self.cache,
            framework_url,
            libs_url,
            skeleton_lib_url=skeleton_lib_url,
            show_progress=True
        )
        framework.ensure_framework()
        return framework

    def _process_libraries(
        self,
        env_config: dict,
        build_dir: Path,
        compiler: ConfigurableCompiler,
        toolchain: ToolchainESP32,
        verbose: bool,
        project_dir: Optional[Path] = None
    ) -> tuple[List[Path], List[Path]]:
        """
        Process and compile library dependencies.

        Args:
            env_config: Environment configuration
            build_dir: Build directory
            compiler: Configured compiler instance
            toolchain: ESP32 toolchain instance
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

        log_phase(8, 13, "Processing library dependencies...")

        # Parse lib_deps (can be string or list)
        if isinstance(lib_deps, str):
            lib_specs = [dep.strip() for dep in lib_deps.split('\n') if dep.strip()]
        else:
            lib_specs = lib_deps

        if not lib_specs:
            return library_archives, library_include_paths

        # Initialize library manager with project directory for resolving local paths
        lib_manager = LibraryManagerESP32(build_dir, project_dir=project_dir)

        # Get compiler flags for library compilation
        lib_compiler_flags = compiler.get_base_flags()

        # Get include paths for library compilation
        lib_include_paths = compiler.get_include_paths()

        # Get toolchain bin path
        toolchain_bin_path = toolchain.get_bin_path()
        if toolchain_bin_path is None:
            log_warning("Toolchain bin directory not found, skipping libraries")
            return library_archives, library_include_paths

        # Extract trampoline cache from compilation executor
        trampoline_cache = None
        if hasattr(compiler, 'compilation_executor') and compiler.compilation_executor:
            trampoline_cache = getattr(compiler.compilation_executor, 'trampoline_cache', None)

        # Ensure libraries are downloaded and compiled
        # Always show progress for library compilation - compiling 300+ files
        # without feedback is confusing UX, even in non-verbose mode
        logger.debug(f"[ORCHESTRATOR] Calling lib_manager.ensure_libraries with {len(lib_specs)} specs: {lib_specs}")
        libraries = lib_manager.ensure_libraries(
            lib_specs,
            toolchain_bin_path,
            lib_compiler_flags,
            lib_include_paths,
            show_progress=True,
            trampoline_cache=trampoline_cache,
        )
        logger.debug(f"[ORCHESTRATOR] ensure_libraries returned {len(libraries)} libraries")

        # Get library archives and include paths
        library_archives = [lib.archive_file for lib in libraries if lib.is_compiled]
        library_include_paths = lib_manager.get_library_include_paths()

        log_detail(f"Compiled {len(libraries)} library dependencies", verbose_only=True)

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
            start_time: Build start time for error reporting
            verbose: Verbose output mode
            src_dir_override: Optional source directory override (relative to project_dir)

        Returns:
            List of compiled object files or None if no sketch found
        """
        log_phase(9, 13, "Compiling sketch...")

        # Determine source directory
        if src_dir_override:
            src_dir = project_dir / src_dir_override
            log_detail(f"Using source directory override: {src_dir_override}", verbose_only=True)
        else:
            src_dir = project_dir

        # Look for .ino files in the source directory
        sketch_files = list(src_dir.glob("*.ino"))
        if not sketch_files:
            return None

        sketch_path = sketch_files[0]
        sketch_obj_files = compiler.compile_sketch(sketch_path)

        log_detail(f"Compiled {len(sketch_obj_files)} sketch file(s)", verbose_only=True)

        return sketch_obj_files

    def _create_bt_stub(
        self,
        build_dir: Path,
        compiler: ConfigurableCompiler,
        verbose: bool
    ) -> Optional[Path]:
        """
        Create a Bluetooth stub for ESP32 targets where esp32-hal-bt.c fails to compile.

        On non-ESP32 targets (ESP32-C6, ESP32-S3, etc.), the esp32-hal-bt.c file may
        fail to compile due to SDK incompatibilities, but initArduino() still references
        btInUse(). This creates a stub implementation that returns false.

        Args:
            build_dir: Build directory
            compiler: Configured compiler instance
            verbose: Whether to print verbose output

        Returns:
            Path to compiled stub object file, or None on error
        """
        try:
            # Create stub source file
            stub_dir = build_dir / "stubs"
            stub_dir.mkdir(parents=True, exist_ok=True)
            stub_file = stub_dir / "bt_stub.c"

            # Write minimal btInUse() implementation
            stub_content = """// Bluetooth stub for ESP32 targets where esp32-hal-bt.c fails to compile
// This provides a fallback implementation of btInUse() that always returns false

#include <stdbool.h>

// Weak attribute allows this to be overridden if the real implementation links
__attribute__((weak)) bool btInUse(void) {
    return false;
}
"""
            stub_file.write_text(stub_content)

            # Compile the stub
            stub_obj = stub_dir / "bt_stub.o"
            compiled_obj = compiler.compile_source(stub_file, stub_obj)

            log_detail(f"Created Bluetooth stub: {compiled_obj.name}", verbose_only=True)

            return compiled_obj

        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            log_warning(f"Failed to create Bluetooth stub: {e}")
            return None

    def _generate_boot_components(
        self,
        linker: ConfigurableLinker,
        mcu: str,
        verbose: bool
    ) -> tuple[Optional[Path], Optional[Path]]:
        """
        Generate bootloader and partition table for ESP32.

        Args:
            linker: Configured linker instance
            mcu: MCU identifier
            verbose: Verbose output mode

        Returns:
            Tuple of (bootloader_bin, partitions_bin)
        """
        bootloader_bin = None
        partitions_bin = None

        if not mcu.startswith("esp32"):
            return bootloader_bin, partitions_bin

        log_phase(12, 13, "Generating bootloader...")
        try:
            bootloader_bin = linker.generate_bootloader()
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            log_warning(f"Could not generate bootloader: {e}")

        log_phase(13, 13, "Generating partition table...")
        try:
            partitions_bin = linker.generate_partition_table()
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            log_warning(f"Could not generate partition table: {e}")

        return bootloader_bin, partitions_bin

    def _print_success(
        self,
        build_time: float,
        firmware_elf: Path,
        firmware_bin: Path,
        bootloader_bin: Optional[Path],
        partitions_bin: Optional[Path],
        merged_bin: Optional[Path],
        size_info: Optional[SizeInfo] = None
    ) -> None:
        """
        Print build success message.

        Args:
            build_time: Total build time
            firmware_elf: Path to firmware ELF
            firmware_bin: Path to firmware binary
            bootloader_bin: Optional path to bootloader
            partitions_bin: Optional path to partition table
            merged_bin: Optional path to merged binary
            size_info: Optional size information to display
        """
        # Build success message
        message_lines = ["BUILD SUCCESSFUL!"]
        message_lines.append(f"Build time: {build_time:.2f}s")
        message_lines.append(f"Firmware ELF: {firmware_elf}")
        message_lines.append(f"Firmware BIN: {firmware_bin}")
        if bootloader_bin:
            message_lines.append(f"Bootloader: {bootloader_bin}")
        if partitions_bin:
            message_lines.append(f"Partitions: {partitions_bin}")
        if merged_bin:
            message_lines.append(f"Merged BIN: {merged_bin}")

        BannerFormatter.print_banner("\n".join(message_lines), width=60, center=False)

        # Print size information if available
        if size_info:
            print()
            from .build_utils import SizeInfoPrinter
            SizeInfoPrinter.print_size_info(size_info)
            print()

    def _error_result(self, start_time: float, message: str) -> BuildResultESP32:
        """
        Create an error result.

        Args:
            start_time: Build start time
            message: Error message

        Returns:
            BuildResultESP32 indicating failure
        """
        return BuildResultESP32(
            success=False,
            firmware_bin=None,
            firmware_elf=None,
            bootloader_bin=None,
            partitions_bin=None,
            merged_bin=None,
            size_info=None,
            build_time=time.time() - start_time,
            message=message
        )

    @staticmethod
    def _resolve_platform_url(platform_spec: str) -> str:
        """
        Resolve platform specification to actual download URL.

        PlatformIO supports several formats for specifying platforms:
        - Full URL: "https://github.com/.../platform-espressif32.zip" -> used as-is
        - Shorthand: "platformio/espressif32" -> resolved to pioarduino stable release
        - Name only: "espressif32" -> resolved to pioarduino stable release

        Args:
            platform_spec: Platform specification from platformio.ini

        Returns:
            Actual download URL for the platform
        """
        # Default stable release URL for espressif32 (pioarduino fork)
        # This is the recommended platform for ESP32 Arduino development
        DEFAULT_ESP32_URL = "https://github.com/pioarduino/platform-espressif32/releases/download/stable/platform-espressif32.zip"

        # If it's already a proper URL, use it as-is
        if platform_spec.startswith("http://") or platform_spec.startswith("https://"):
            return platform_spec

        # Handle PlatformIO shorthand formats
        if platform_spec in ("platformio/espressif32", "espressif32"):
            log_detail(f"Resolving platform shorthand '{platform_spec}' to pioarduino stable release")
            return DEFAULT_ESP32_URL

        # For unknown formats, return as-is and let the download fail with a clear error
        log_warning(f"Unknown platform format: {platform_spec}, attempting to use as URL")
        return platform_spec
