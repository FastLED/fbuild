"""
ESP32-specific build orchestration for Fbuild projects.

This module handles ESP32 platform builds separately from AVR builds,
providing cleaner separation of concerns and better maintainability.
"""

import _thread
import logging
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, List, Optional

if TYPE_CHECKING:
    from fbuild.build.build_context import BuildParams

from fbuild.build.build_info_generator import BuildInfoGenerator
from fbuild.build.build_state import BuildStateTracker
from fbuild.build.configurable_compiler import ConfigurableCompiler
from fbuild.build.configurable_linker import ConfigurableLinker
from fbuild.build.linker import SizeInfo
from fbuild.build.orchestrator import BuildResult, IBuildOrchestrator
from fbuild.build.psram_utils import board_has_psram, get_psram_mode
from fbuild.cli_utils import BannerFormatter
from fbuild.output import DefaultProgressCallback, log_detail, log_phase, log_warning
from fbuild.packages import Cache
from fbuild.packages.framework_esp32 import FrameworkESP32
from fbuild.packages.library_manager_esp32 import LibraryESP32, LibraryManagerESP32
from fbuild.packages.platform_esp32 import PlatformESP32
from fbuild.packages.toolchain_esp32 import ToolchainESP32

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
        from fbuild.config import PlatformIOConfig

        # Extract from request
        project_dir = request.project_dir
        env_name = request.env_name

        # Parse platformio.ini to get environment configuration
        ini_path = project_dir / "platformio.ini"
        if not ini_path.exists():
            return BuildResult(success=False, hex_path=None, elf_path=None, size_info=None, build_time=0.0, message=f"platformio.ini not found in {project_dir}")

        try:
            config = PlatformIOConfig(ini_path)

            env_config = config.get_env_config(env_name)
            board_id = env_config.get("board", "")
            build_flags = config.get_build_flags(env_name)

            # Add debug logging for lib_deps
            logger.debug(f"[ORCHESTRATOR] About to call config.get_lib_deps('{env_name}')")
            lib_deps = config.get_lib_deps(env_name)
            logger.debug(f"[ORCHESTRATOR] get_lib_deps returned: {lib_deps}")

            # Filter out ignored libraries (lib_ignore from platformio.ini)
            lib_ignore_str = env_config.get("lib_ignore", "")
            if lib_ignore_str:
                ignored_names = {name.strip().lower() for name in lib_ignore_str.replace("\n", ",").split(",") if name.strip()}
                original_count = len(lib_deps)
                lib_deps = [dep for dep in lib_deps if not any(ignored in dep.lower() for ignored in ignored_names)]
                if len(lib_deps) < original_count:
                    logger.debug(f"[ORCHESTRATOR] lib_ignore filtered {original_count - len(lib_deps)} deps: {ignored_names}")

            # Parse and apply build_unflags (removes matching flags from build_flags)
            build_unflags_str = env_config.get("build_unflags", "")
            if build_unflags_str:
                unflags = {f.strip() for f in build_unflags_str.split() if f.strip()}
                original_count = len(build_flags)
                build_flags = [f for f in build_flags if f not in unflags]
                if len(build_flags) < original_count:
                    logger.debug(f"[ORCHESTRATOR] build_unflags removed {original_count - len(build_flags)} flags: {unflags}")

            # Call internal build method
            esp32_result = self._build_esp32(board_id, env_config, build_flags, lib_deps, request)

            # Convert BuildResultESP32 to BuildResult
            return BuildResult(
                success=esp32_result.success,
                hex_path=esp32_result.firmware_bin,
                elf_path=esp32_result.firmware_elf,
                size_info=esp32_result.size_info,
                build_time=esp32_result.build_time,
                message=esp32_result.message,
            )

        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception as e:
            return BuildResult(success=False, hex_path=None, elf_path=None, size_info=None, build_time=0.0, message=f"Failed to parse configuration: {e}")

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
            # Garbage-collect old stderr dirs from force-killed builds (best-effort, 24h cutoff)
            from fbuild.packages.library_manager_esp32 import garbage_collect_stderr_dirs

            garbage_collect_stderr_dirs(build_dir.parent, max_age_hours=24)

            # Get platform URL from env_config
            platform_url = env_config.get("platform")
            if not platform_url:
                return self._error_result(start_time, "No platform URL specified in platformio.ini")

            # Resolve platform shorthand to actual download URL
            # PlatformIO supports formats like "platformio/espressif32" which need
            # to be converted to a real download URL
            platform_url = self._resolve_platform_url(platform_url)

            # Print build profile banner
            from fbuild.build.build_profiles import print_profile_banner

            print_profile_banner(request.profile)

            # Start library pre-fetch immediately - lib_deps from platformio.ini
            # need no platform/toolchain info, so downloads overlap with everything
            lib_prefetch_thread = None
            if lib_deps:

                def _prefetch_libs_early() -> None:
                    try:
                        from fbuild.packages.platformio_registry import LibrarySpec

                        build_dir.mkdir(parents=True, exist_ok=True)
                        lib_mgr = LibraryManagerESP32(build_dir, project_dir=project_dir)
                        parsed = [LibrarySpec.parse(s) for s in lib_deps]
                        max_w = min(len(parsed), 8)
                        if max_w <= 1:
                            for spec in parsed:
                                lib_mgr.download_library(spec, True)
                        else:
                            from concurrent.futures import ThreadPoolExecutor, as_completed

                            with ThreadPoolExecutor(max_workers=max_w) as dl_exec:
                                futs = {dl_exec.submit(lib_mgr.download_library, spec, True): spec for spec in parsed}
                                for fut in as_completed(futs):
                                    try:
                                        fut.result()
                                    except KeyboardInterrupt:
                                        raise
                                    except Exception:
                                        pass  # Non-fatal: will retry in ensure_libraries
                    except KeyboardInterrupt:
                        raise
                    except Exception as e:
                        log_detail(f"Library pre-fetch warning: {e}")

                lib_prefetch_thread = threading.Thread(target=_prefetch_libs_early, daemon=True)
                lib_prefetch_thread.start()
                log_detail("Started library pre-fetch in background")

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

            # Initialize toolchain and framework in parallel (they are independent)
            # Library pre-fetch already started above, overlapping with platform download
            toolchain, framework = self._setup_toolchain_and_framework_parallel(
                packages,
                start_time,
                verbose,
                mcu,
            )
            if toolchain is None:
                return self._error_result(start_time, "Failed to initialize toolchain")
            if framework is None:
                return self._error_result(start_time, "Failed to initialize framework")

            # Ensure build directory exists
            build_dir.mkdir(parents=True, exist_ok=True)

            # Wait for library pre-fetch to complete before build state check
            # (build state check may wipe build_dir, so prefetch must finish first)
            if lib_prefetch_thread is not None:
                lib_prefetch_thread.join(timeout=300)

            # Determine source directory for cache invalidation
            # This is computed early to include source file changes in cache key
            from fbuild.config import PlatformIOConfig

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
                framework=env_config.get("framework", "arduino"),
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
                from fbuild.build.build_utils import safe_rmtree

                if build_dir.exists():
                    safe_rmtree(build_dir)
                # Recreate build directory
                build_dir.mkdir(parents=True, exist_ok=True)
            else:
                log_detail("Build configuration unchanged, using cached artifacts", verbose_only=True)

            # Initialize compilation executor early to show sccache status
            from fbuild.build.compilation_executor import CompilationExecutor

            compilation_executor = CompilationExecutor(
                build_dir=build_dir,
                show_progress=verbose,
                compile_database=request.compile_database,
                execute_compilations=not request.generate_compiledb,
            )

            # Load platform configuration ONCE (not redundantly in compiler/linker)
            from fbuild import platform_configs

            platform_config = platform_configs.load_config(mcu)
            if platform_config is None:
                return self._error_result(start_time, f"No platform configuration found for {mcu}. Available: {platform_configs.list_available_configs()}")

            # Extract variant and core from board config
            variant = board_json.get("build", {}).get("variant", "")
            core = board_json.get("build", {}).get("core", "arduino")

            # Create full BuildContext with all configuration loaded once
            from fbuild.build.build_context import BuildContext

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
            log_phase(7, 13, "Compiling core + libraries (parallel)...")

            compiler = ConfigurableCompiler(context)

            # Core and user libraries compile in parallel - they use independent
            # compilation mechanisms and write to separate directories:
            #   Core: compiler's async queue -> build_dir/core/
            #   Libraries: flat ThreadPoolExecutor -> build_dir/libs/

            def _compile_core_phase() -> Path:
                progress_callback = DefaultProgressCallback(verbose_only=not verbose)
                if verbose:
                    core_obj_files = compiler.compile_core(progress_callback=progress_callback)
                else:
                    from tqdm import tqdm

                    core_sources = framework.get_core_sources(compiler.core)
                    total_files = len(core_sources)
                    with tqdm(total=total_files, desc="Compiling Arduino core", unit="file", ncols=80, leave=False) as pbar:
                        core_obj_files = compiler.compile_core(progress_bar=pbar, progress_callback=progress_callback)
                    log_detail(f"Compiled {len(core_obj_files)} core files")

                bt_stub_obj = self._create_bt_stub(build_dir, compiler, verbose)
                if bt_stub_obj:
                    core_obj_files.append(bt_stub_obj)

                if hasattr(compiler, "wait_all_jobs"):
                    compiler.wait_all_jobs()

                return compiler.create_core_archive(core_obj_files)

            def _process_libs_phase() -> tuple[List[Path], List[Path]]:
                return self._process_libraries(env_config, build_dir, compiler, toolchain, verbose, project_dir=project_dir)

            from concurrent.futures import Future, ThreadPoolExecutor

            phase_executor = ThreadPoolExecutor(max_workers=2)
            try:
                core_future: Future[Path] = phase_executor.submit(_compile_core_phase)
                libs_future: Future[tuple[List[Path], List[Path]]] = phase_executor.submit(_process_libs_phase)

                core_archive = core_future.result()
                library_archives, library_include_paths = libs_future.result()
            except KeyboardInterrupt as ke:
                phase_executor.shutdown(wait=False, cancel_futures=True)
                from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

                handle_keyboard_interrupt_properly(ke)
                raise  # Never reached
            except Exception:
                phase_executor.shutdown(wait=False, cancel_futures=True)
                raise
            finally:
                phase_executor.shutdown(wait=False)

            log_detail("Core + library compilation complete", verbose_only=True)

            # Add library include paths to compiler
            if library_include_paths:
                compiler.add_library_includes(library_include_paths)

            # src_dir_override was computed earlier for cache invalidation

            # Compile sketch and framework libraries IN PARALLEL.
            # These are independent: sketch needs only header include paths (already set),
            # framework libs need only the toolchain + framework source.
            # Pre-capture compiler state BEFORE spawning threads to avoid race condition
            # with sketch's add_sketch_include() — framework code must NOT see sketch paths.
            from concurrent.futures import Future
            from concurrent.futures import ThreadPoolExecutor as SketchFwExecutor

            fw_pre_flags = compiler.get_base_flags()
            fw_pre_includes = list(compiler.get_include_paths())

            def _compile_sketch_phase() -> Optional[List[Path]]:
                return self._compile_sketch(project_dir, compiler, start_time, verbose, src_dir_override)

            def _compile_fw_libs_phase() -> tuple[List[Path], List[Path]]:
                return self._compile_all_framework_libraries(
                    framework,
                    build_dir,
                    library_archives,
                    toolchain,
                    compiler,
                    verbose,
                    pre_captured_flags=fw_pre_flags,
                    pre_captured_includes=fw_pre_includes,
                )

            sketch_fw_executor = SketchFwExecutor(max_workers=2)
            try:
                # Launch framework libs FIRST so it captures compiler state before
                # sketch's add_sketch_include() modifies include paths
                fw_future: Future[tuple[List[Path], List[Path]]] = sketch_fw_executor.submit(_compile_fw_libs_phase)
                sketch_future: Future[Optional[List[Path]]] = sketch_fw_executor.submit(_compile_sketch_phase)

                sketch_obj_files = sketch_future.result()
                if sketch_obj_files is None:
                    fw_future.cancel()
                    search_dir = project_dir / src_dir_override if src_dir_override else project_dir
                    return self._error_result(start_time, f"No .ino sketch file found in {search_dir}")

                fw_archives, fw_includes = fw_future.result()
            except KeyboardInterrupt as ke:
                sketch_fw_executor.shutdown(wait=False, cancel_futures=True)
                from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

                handle_keyboard_interrupt_properly(ke)
                raise  # Never reached
            except Exception:
                sketch_fw_executor.shutdown(wait=False, cancel_futures=True)
                raise
            finally:
                sketch_fw_executor.shutdown(wait=False)

            library_archives.extend(fw_archives)
            if fw_includes:
                library_include_paths.extend(fw_includes)

            # In compiledb-only mode, skip linking — we only need compile commands
            if request.generate_compiledb:
                build_time = time.time() - start_time
                return BuildResultESP32(
                    success=True,
                    firmware_bin=None,
                    firmware_elf=None,
                    bootloader_bin=None,
                    partitions_bin=None,
                    merged_bin=None,
                    size_info=None,
                    build_time=build_time,
                    message="compile_commands.json generated (compiledb mode)",
                )

            # Process embedded files (board_build.embed_files / board_build.embed_txtfiles)
            embed_obj_files = self._process_embed_files(env_config, project_dir, build_dir, toolchain, mcu, verbose)
            if embed_obj_files:
                sketch_obj_files.extend(embed_obj_files)

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
            bootloader_bin, partitions_bin = self._generate_boot_components(linker, mcu, verbose)

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
                self._print_success(build_time, firmware_elf, firmware_bin, bootloader_bin, partitions_bin, merged_bin, size_info)

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
                message="Build successful (native ESP32 build)",
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
                message=f"ESP32 native build failed: {e}\n\n{error_trace}",
            )

    def _setup_toolchain(
        self,
        packages: dict,
        start_time: float,
        verbose: bool,
        mcu: Optional[str] = None,
    ) -> Optional["ToolchainESP32"]:
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

    def _setup_framework(self, packages: dict, start_time: float, verbose: bool) -> Optional[FrameworkESP32]:
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

        framework = FrameworkESP32(self.cache, framework_url, libs_url, skeleton_lib_url=skeleton_lib_url, show_progress=True)
        framework.ensure_framework()
        return framework

    def _setup_toolchain_and_framework_parallel(
        self,
        packages: dict,
        start_time: float,
        verbose: bool,
        mcu: str,
    ) -> tuple[Optional["ToolchainESP32"], Optional[FrameworkESP32]]:
        """
        Initialize toolchain and framework in parallel.

        Platform must already be initialized. Library pre-fetch is handled
        separately in _build_esp32 (starts before platform download).

        Args:
            packages: Package URLs dictionary from platform
            start_time: Build start time for error reporting
            verbose: Verbose output mode
            mcu: Target MCU identifier

        Returns:
            Tuple of (toolchain, framework), either can be None on failure
        """
        from concurrent.futures import Future, ThreadPoolExecutor

        log_phase(4, 13, "Initializing toolchain + framework (parallel)...")

        toolchain_result: Optional["ToolchainESP32"] = None
        framework_result: Optional[FrameworkESP32] = None
        toolchain_error: Optional[Exception] = None
        framework_error: Optional[Exception] = None

        def setup_toolchain() -> Optional["ToolchainESP32"]:
            toolchain_url = packages.get("toolchain-riscv32-esp") or packages.get("toolchain-xtensa-esp-elf")
            if not toolchain_url:
                return None
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

        def setup_framework() -> Optional[FrameworkESP32]:
            framework_url = packages.get("framework-arduinoespressif32")
            libs_url = packages.get("framework-arduinoespressif32-libs", "")
            if not framework_url:
                return None
            skeleton_lib_url = None
            for package_name, package_url in packages.items():
                if package_name.startswith("framework-arduino-") and package_name.endswith("-skeleton-lib"):
                    skeleton_lib_url = package_url
                    break
            framework = FrameworkESP32(self.cache, framework_url, libs_url, skeleton_lib_url=skeleton_lib_url, show_progress=True)
            framework.ensure_framework()
            return framework

        executor = ThreadPoolExecutor(max_workers=2)
        try:
            toolchain_future: Future[Optional["ToolchainESP32"]] = executor.submit(setup_toolchain)
            framework_future: Future[Optional[FrameworkESP32]] = executor.submit(setup_framework)

            try:
                toolchain_result = toolchain_future.result()
            except KeyboardInterrupt:
                raise
            except Exception as e:
                toolchain_error = e
                log_warning(f"Toolchain setup failed: {e}")

            try:
                framework_result = framework_future.result()
            except KeyboardInterrupt:
                raise
            except Exception as e:
                framework_error = e
                log_warning(f"Framework setup failed: {e}")
        except KeyboardInterrupt as ke:
            executor.shutdown(wait=False, cancel_futures=True)
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached
        finally:
            executor.shutdown(wait=False)

        if toolchain_error:
            raise toolchain_error
        if framework_error:
            raise framework_error

        return toolchain_result, framework_result

    def _process_libraries(
        self, env_config: dict, build_dir: Path, compiler: ConfigurableCompiler, toolchain: ToolchainESP32, verbose: bool, project_dir: Optional[Path] = None
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
        lib_deps = env_config.get("lib_deps", "")
        library_archives = []
        library_include_paths = []

        if not lib_deps:
            return library_archives, library_include_paths

        log_phase(8, 13, "Processing library dependencies...")

        # Parse lib_deps (can be string or list)
        if isinstance(lib_deps, str):
            lib_specs = [dep.strip() for dep in lib_deps.split("\n") if dep.strip()]
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

        # Parse lib_ignore for transitive dependency filtering
        lib_ignore_str = env_config.get("lib_ignore", "")
        lib_ignore_set: Optional[set[str]] = None
        if lib_ignore_str:
            lib_ignore_set = {name.strip().lower() for name in lib_ignore_str.replace("\n", ",").split(",") if name.strip()}

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
            lib_ignore=lib_ignore_set,
        )
        logger.debug(f"[ORCHESTRATOR] ensure_libraries returned {len(libraries)} libraries")

        # Get library archives and include paths
        library_archives = [lib.archive_file for lib in libraries if lib.is_compiled]
        library_include_paths = lib_manager.get_library_include_paths()

        log_detail(f"Compiled {len(libraries)} library dependencies", verbose_only=True)

        return library_archives, library_include_paths

    def _compile_all_framework_libraries(
        self,
        framework: FrameworkESP32,
        build_dir: Path,
        existing_archives: List[Path],
        toolchain: ToolchainESP32,
        compiler: ConfigurableCompiler,
        verbose: bool,
        pre_captured_flags: List[str] | None = None,
        pre_captured_includes: List[Path] | None = None,
    ) -> tuple[List[Path], List[Path]]:
        """
        Compile ALL framework built-in libraries (BLE, WiFi, SPI, Wire, etc.).

        Instead of scanning #include directives to detect which libraries are needed,
        we compile every library in the framework's libraries/ directory. The linker
        with --gc-sections strips all unreferenced code, so final binary size is identical.

        This approach is simpler, more correct (never misses a library regardless of
        conditional compilation or #ifdef guards), and cacheable (compile once, reuse).

        Args:
            framework: ESP32 framework instance
            build_dir: Build directory
            existing_archives: Already-compiled library archives (to avoid re-compilation)
            toolchain: ESP32 toolchain instance
            compiler: Configured compiler instance
            verbose: Verbose output mode

        Returns:
            Tuple of (additional_archives, additional_include_paths)
        """
        libraries_dir = framework.get_libraries_dir()
        if not libraries_dir.exists():
            return [], []

        # Build set of already-compiled library names from existing archives
        # Archive names are lib<name>.a where <name> is the sanitized library name
        already_compiled: set[str] = set()
        for archive in existing_archives:
            archive_name = archive.stem  # e.g., "libfastled" -> stem is "libfastled"
            if archive_name.startswith("lib"):
                already_compiled.add(archive_name[3:])  # strip "lib" prefix

        # Enumerate ALL framework libraries
        all_libs: List[tuple[str, Path]] = []
        for lib_dir in sorted(libraries_dir.iterdir()):
            if not lib_dir.is_dir() or lib_dir.name.startswith("."):
                continue
            src_dir = lib_dir / "src"
            if not src_dir.exists():
                continue
            sanitized = lib_dir.name.lower().replace("/", "_").replace("@", "_")
            if sanitized in already_compiled:
                continue
            all_libs.append((lib_dir.name, lib_dir))

        if not all_libs:
            return [], []

        log_detail(f"Compiling {len(all_libs)} framework libraries (linker will strip unused code)")

        # Compile framework libraries using LibraryManagerESP32
        lib_manager = LibraryManagerESP32(build_dir, project_dir=build_dir)

        toolchain_bin_path = toolchain.get_bin_path()
        if toolchain_bin_path is None:
            log_warning("Toolchain bin directory not found, skipping framework libraries")
            return [], []

        lib_compiler_flags = pre_captured_flags if pre_captured_flags is not None else compiler.get_base_flags()
        lib_include_paths = list(pre_captured_includes) if pre_captured_includes is not None else list(compiler.get_include_paths())

        # Add ALL framework library include paths upfront so that inter-library
        # dependencies resolve (e.g., BLE includes <NetworkClient.h> from Network).
        # Without this, compilation fails for libraries that reference sibling headers.
        for _lib_name, fw_lib_root in all_libs:
            fw_src = fw_lib_root / "src"
            if fw_src.is_dir():
                lib_include_paths.append(fw_src)

        additional_archives: List[Path] = []
        additional_includes: List[Path] = []

        # Phase 1: Setup all libraries, collect compile jobs into a single flat pool
        # This replaces per-library sequential compilation with one big parallel pool
        # mapping: sanitized_name -> (lib_name, LibraryESP32, expected obj files)
        lib_info: dict[str, tuple[str, LibraryESP32, list[Path]]] = {}
        all_compile_jobs: list[tuple[str, Path, Path, list[str]]] = []  # (sanitized_name, source, obj, cmd)

        for lib_name, fw_lib_root in all_libs:
            try:
                sanitized_name = lib_name.lower().replace("/", "_").replace("@", "_")
                lib_dir = lib_manager.libs_dir / sanitized_name
                library = LibraryESP32(lib_dir, sanitized_name)

                # Set up the library directory by copying/symlinking from framework
                if not library.exists:
                    self._setup_framework_library(library, fw_lib_root)

                # Check if rebuild needed
                needs_rebuild_flag, _reason = lib_manager.needs_rebuild(library, lib_compiler_flags)

                if not needs_rebuild_flag:
                    if library.is_compiled:
                        additional_archives.append(library.archive_file)
                    additional_includes.extend(library.get_include_dirs())
                    continue

                # Prepare compile jobs (returns empty list for header-only libs)
                jobs = lib_manager.prepare_compile_jobs(library, toolchain_bin_path, lib_compiler_flags, lib_include_paths)
                if not jobs:
                    log_detail(f"Framework library '{lib_name}' is header-only, skipping compilation", verbose_only=True)
                    additional_includes.extend(library.get_include_dirs())
                    continue

                log_detail(f"Compiling framework library: {lib_name} ({len(jobs)} files)")
                lib_info[sanitized_name] = (lib_name, library, [])
                for source, obj_file, cmd in jobs:
                    all_compile_jobs.append((sanitized_name, source, obj_file, cmd))

            except KeyboardInterrupt as ke:
                from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

                handle_keyboard_interrupt_properly(ke)
                raise  # Never reached, but satisfies type checker
            except Exception as e:
                log_warning(f"Failed to set up framework library '{lib_name}': {e}")

        # Phase 2: Compile ALL framework library files through the compiler's daemon
        # queue — the SAME pool used by sketch and core compilation. This avoids
        # CPU contention from running separate thread pools in parallel.
        if all_compile_jobs and compiler.compilation_queue:
            total = len(all_compile_jobs)
            num_workers = compiler.compilation_queue.num_workers

            log_detail(f"Compiling {total} framework source files across {len(lib_info)} libraries ({num_workers} workers)")

            # Submit all jobs directly to the daemon queue (bypassing compiler's
            # pending_jobs list to avoid race with sketch compilation thread)
            import time as _time

            from fbuild.daemon.compilation_queue import CompilationJob

            fw_job_ids: list[tuple[str, str, Path]] = []  # (job_id, sanitized_name, source)
            for sanitized_name, source, obj_file, cmd in all_compile_jobs:
                job_id = f"fw_{source.stem}_{int(_time.time() * 1000000)}"
                job = CompilationJob(
                    job_id=job_id,
                    source_path=source,
                    output_path=obj_file,
                    compiler_cmd=cmd,
                )
                compiler.compilation_queue.submit_job(job)
                fw_job_ids.append((job_id, sanitized_name, source))

            # Wait for all framework jobs to complete
            all_fw_ids = [jid for jid, _, _ in fw_job_ids]
            compiler.compilation_queue.wait_for_completion(all_fw_ids)

            # Collect results and report progress
            completed = 0
            failed_jobs: list[str] = []
            for job_id, sanitized_name, source in fw_job_ids:
                completed += 1
                job = compiler.compilation_queue.get_job_status(job_id)
                if job is not None and job.state.value == "completed":
                    lib_info[sanitized_name][2].append(job.output_path)
                    log_detail(f"[{completed}/{total}] {source.name}", indent=8)
                elif job is not None:
                    failed_jobs.append(f"{source.name}: {job.stderr[:4000]}")
                    log_detail(f"[{completed}/{total}] {source.name} FAILED", indent=8)

            if failed_jobs:
                from fbuild.build.configurable_compiler import ConfigurableCompilerError

                error_msg = f"Framework compilation failed for {len(failed_jobs)} file(s):\n"
                error_msg += "\n".join(f"  - {err}" for err in failed_jobs[:10])
                raise ConfigurableCompilerError(error_msg)

        # Phase 3: Archive each library from its compiled object files (parallel)
        archive_tasks = [(sanitized_name, lib_name, library, object_files) for sanitized_name, (lib_name, library, object_files) in lib_info.items() if object_files]

        # Include dirs from libraries with no object files (header-only after job prep)
        for sanitized_name, (lib_name, library, object_files) in lib_info.items():
            if not object_files:
                additional_includes.extend(library.get_include_dirs())

        if len(archive_tasks) <= 1:
            for sanitized_name, lib_name, library, object_files in archive_tasks:
                try:
                    log_detail(f"Creating archive: lib{sanitized_name}.a", indent=8)
                    lib_manager.archive_library(library, toolchain_bin_path, object_files, lib_compiler_flags)
                    if library.is_compiled:
                        additional_archives.append(library.archive_file)
                    additional_includes.extend(library.get_include_dirs())
                except KeyboardInterrupt as ke:
                    from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

                    handle_keyboard_interrupt_properly(ke)
                    raise  # Never reached
                except Exception as e:
                    log_warning(f"Failed to archive framework library '{lib_name}': {e}")
        elif archive_tasks:

            def _do_archive(s_name: str, l_name: str, lib: LibraryESP32, objs: list) -> tuple[str, str, LibraryESP32]:
                log_detail(f"Creating archive: lib{s_name}.a", indent=8)
                lib_manager.archive_library(lib, toolchain_bin_path, objs, lib_compiler_flags)
                return s_name, l_name, lib

            from concurrent.futures import ThreadPoolExecutor as ArExecutorPool

            ar_executor = ArExecutorPool(max_workers=min(len(archive_tasks), 4))
            try:
                # Submit all, keep ordered list for deterministic output
                ar_ordered = [(ar_executor.submit(_do_archive, s_name, l_name, lib, objs), s_name, l_name, lib) for s_name, l_name, lib, objs in archive_tasks]
                for future, _s_name, l_name, lib in ar_ordered:
                    try:
                        future.result()
                        if lib.is_compiled:
                            additional_archives.append(lib.archive_file)
                        additional_includes.extend(lib.get_include_dirs())
                    except KeyboardInterrupt:
                        raise
                    except Exception as e:
                        log_warning(f"Failed to archive framework library '{l_name}': {e}")
            except KeyboardInterrupt as ke:
                ar_executor.shutdown(wait=False, cancel_futures=True)
                from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

                handle_keyboard_interrupt_properly(ke)
                raise  # Never reached
            finally:
                ar_executor.shutdown(wait=False)

        return additional_archives, additional_includes

    @staticmethod
    def _setup_framework_library(library: LibraryESP32, fw_lib_root: Path) -> None:
        """
        Set up a framework library in the build directory.

        Copies the framework library source into the build directory's libs/ folder
        so it can be compiled by LibraryManagerESP32.

        Args:
            library: LibraryESP32 instance to set up
            fw_lib_root: Root directory of the framework library (e.g., libraries/BLE/)
        """
        import shutil

        library.lib_dir.mkdir(parents=True, exist_ok=True)

        # Copy source directory
        fw_src = fw_lib_root / "src"
        source_path = fw_src if fw_src.is_dir() else fw_lib_root

        if library.src_dir.exists():
            shutil.rmtree(library.src_dir)
        shutil.copytree(source_path, library.src_dir)

        # Create minimal library.json metadata
        import json

        metadata = {"name": library.name, "version": "0.0.0", "frameworks": ["arduino"], "source": "framework-builtin"}
        with open(library.info_file, "w", encoding="utf-8") as f:
            json.dump(metadata, f, indent=2)

    def _compile_sketch(self, project_dir: Path, compiler: ConfigurableCompiler, start_time: float, verbose: bool, src_dir_override: Optional[str] = None) -> Optional[List[Path]]:
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
        if sketch_files:
            sketch_path = sketch_files[0]
            sketch_obj_files = compiler.compile_sketch(sketch_path)
            log_detail(f"Compiled {len(sketch_obj_files)} sketch file(s)", verbose_only=True)
            return sketch_obj_files

        # No .ino file found — look for .cpp/.c files in src/ directory
        # PlatformIO projects can use main.cpp with setup()/loop() instead of .ino
        cpp_src_dir = src_dir / "src" if (src_dir / "src").is_dir() else src_dir
        source_files = []
        for pattern in ["**/*.cpp", "**/*.c", "**/*.cc", "**/*.cxx"]:
            source_files.extend(cpp_src_dir.glob(pattern))

        if not source_files:
            return None

        log_detail(f"No .ino found, compiling {len(source_files)} source file(s) from {cpp_src_dir.relative_to(project_dir)}", verbose_only=True)

        # Add project src/ and include/ dirs as sketch includes (prepended
        # before framework/SDK paths) so the sketch's own headers take priority.
        # Library/framework code is compiled separately with its own include
        # paths and never sees these sketch directories.
        compiler.add_sketch_include(cpp_src_dir)
        include_dir = src_dir / "include"
        if include_dir.is_dir():
            compiler.add_sketch_include(include_dir)

        obj_dir = compiler.build_dir / "obj"
        obj_dir.mkdir(parents=True, exist_ok=True)

        object_files = []
        for source_file in source_files:
            rel_path = source_file.relative_to(cpp_src_dir)
            obj_path = obj_dir / rel_path.with_suffix(".o")
            obj_path.parent.mkdir(parents=True, exist_ok=True)

            if not compiler.needs_rebuild(source_file, obj_path):
                object_files.append(obj_path)
                continue

            try:
                compiled_obj = compiler.compile_source(source_file, obj_path)
                object_files.append(compiled_obj)
            except KeyboardInterrupt:
                raise
            except Exception as e:
                from fbuild.build.configurable_compiler import ConfigurableCompilerError

                raise ConfigurableCompilerError(f"Failed to compile {source_file.name}: {e}")

        compiler.wait_all_jobs()
        log_detail(f"Compiled {len(object_files)} source file(s)", verbose_only=True)
        return object_files

    def _create_bt_stub(self, build_dir: Path, compiler: ConfigurableCompiler, verbose: bool) -> Optional[Path]:
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

    def _process_embed_files(
        self,
        env_config: dict,
        project_dir: Path,
        build_dir: Path,
        toolchain: ToolchainESP32,
        mcu: str,
        verbose: bool,
    ) -> List[Path]:
        """Convert board_build.embed_files and board_build.embed_txtfiles to linkable object files.

        PlatformIO's embed_files/embed_txtfiles feature converts arbitrary files into
        linkable objects with _binary_<name>_start/_end/_size symbols. embed_txtfiles
        additionally appends a null terminator so the data can be used as a C string.

        Args:
            env_config: Environment configuration dict
            project_dir: Project root directory (files are relative to this)
            build_dir: Build output directory
            toolchain: ESP32 toolchain (provides objcopy)
            mcu: MCU identifier (e.g., "esp32", "esp32c6")
            verbose: Verbose output mode

        Returns:
            List of object file paths to include in linking
        """
        from fbuild.subprocess_utils import safe_run

        embed_files_str = env_config.get("board_build.embed_files", "")
        embed_txtfiles_str = env_config.get("board_build.embed_txtfiles", "")

        logging.debug(f"[EMBED] embed_files='{embed_files_str}', embed_txtfiles='{embed_txtfiles_str}'")

        if not embed_files_str and not embed_txtfiles_str:
            logging.debug("[EMBED] No embed files configured, skipping")
            return []

        objcopy_path = toolchain.get_objcopy_path()
        if objcopy_path is None:
            log_warning("objcopy not found, skipping embedded files")
            return []

        # Determine architecture-specific objcopy flags
        is_riscv = mcu in ("esp32c2", "esp32c3", "esp32c6", "esp32h2")
        if is_riscv:
            output_target = "elf32-littleriscv"
            binary_arch = "riscv"
        else:
            output_target = "elf32-xtensa-le"
            binary_arch = "xtensa"

        embed_dir = build_dir / "embed"
        embed_dir.mkdir(parents=True, exist_ok=True)

        object_files: List[Path] = []

        # Process both embed types
        all_entries: List[tuple[str, bool]] = []  # (path, is_txtfile)
        for path_str in embed_files_str.split():
            path_str = path_str.strip()
            if path_str:
                all_entries.append((path_str, False))
        for path_str in embed_txtfiles_str.split():
            path_str = path_str.strip()
            if path_str:
                all_entries.append((path_str, True))

        logging.debug(f"[EMBED] Processing {len(all_entries)} embed entries: {all_entries}")

        for file_path_str, is_txtfile in all_entries:
            source_file = project_dir / file_path_str
            if not source_file.exists():
                logging.warning(f"[EMBED] Embedded file not found: {source_file}")
                continue

            # objcopy derives symbol names from the input file path.
            # To get _binary_config_timezones_json_start from "config/timezones.json",
            # we must run objcopy with the relative path as input, using cwd=project_dir.
            # For txtfiles, we create a null-terminated copy at the same relative path
            # inside the embed directory, then use cwd=embed_dir.
            if is_txtfile:
                # Create null-terminated copy at same relative path inside embed_dir
                txt_copy = embed_dir / file_path_str
                txt_copy.parent.mkdir(parents=True, exist_ok=True)
                data = source_file.read_bytes()
                txt_copy.write_bytes(data + b"\x00")
                objcopy_cwd = embed_dir
            else:
                objcopy_cwd = project_dir

            # Output object file path
            safe_name = file_path_str.replace("/", "_").replace("\\", "_").replace(".", "_").replace("-", "_")
            obj_file = embed_dir / f"{safe_name}.o"

            # Run objcopy from the correct working directory so the relative path
            # becomes the symbol name prefix (e.g., config/timezones.json -> _binary_config_timezones_json_*)
            cmd = [
                str(objcopy_path),
                "--input-target",
                "binary",
                "--output-target",
                output_target,
                "--binary-architecture",
                binary_arch,
                "--rename-section",
                ".data=.rodata.embedded",
                file_path_str.replace("\\", "/"),
                str(obj_file),
            ]

            if verbose:
                log_detail(f"Embedding file: {file_path_str}")

            logging.debug(f"[EMBED] Running objcopy (cwd={objcopy_cwd}): {' '.join(cmd)}")
            result = safe_run(cmd, capture_output=True, text=True, cwd=str(objcopy_cwd))
            if result.returncode != 0:
                logging.error(f"[EMBED] objcopy failed for {file_path_str}: {result.stderr}")
                continue

            logging.debug(f"[EMBED] Successfully embedded {file_path_str} -> {obj_file}")
            object_files.append(obj_file)

        if object_files:
            logging.info(f"[EMBED] Embedded {len(object_files)} file(s) into firmware")
            log_detail(f"Embedded {len(object_files)} file(s) into firmware")

        return object_files

    def _generate_boot_components(self, linker: ConfigurableLinker, mcu: str, verbose: bool) -> tuple[Optional[Path], Optional[Path]]:
        """
        Generate bootloader and partition table for ESP32 (parallel).

        Bootloader and partition table are independent - they come from the SDK
        and don't depend on each other, so they can be generated simultaneously.

        Args:
            linker: Configured linker instance
            mcu: MCU identifier
            verbose: Verbose output mode

        Returns:
            Tuple of (bootloader_bin, partitions_bin)
        """
        if not mcu.startswith("esp32"):
            return None, None

        log_phase(12, 13, "Generating boot components (parallel)...")

        def _gen_bootloader() -> Optional[Path]:
            try:
                return linker.generate_bootloader()
            except KeyboardInterrupt:
                raise
            except Exception as e:
                log_warning(f"Could not generate bootloader: {e}")
                return None

        def _gen_partitions() -> Optional[Path]:
            try:
                return linker.generate_partition_table()
            except KeyboardInterrupt:
                raise
            except Exception as e:
                log_warning(f"Could not generate partition table: {e}")
                return None

        from concurrent.futures import ThreadPoolExecutor

        boot_executor = ThreadPoolExecutor(max_workers=2)
        try:
            boot_future = boot_executor.submit(_gen_bootloader)
            part_future = boot_executor.submit(_gen_partitions)

            bootloader_bin = boot_future.result()
            partitions_bin = part_future.result()
        except KeyboardInterrupt as ke:
            boot_executor.shutdown(wait=False, cancel_futures=True)
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached
        finally:
            boot_executor.shutdown(wait=False)

        return bootloader_bin, partitions_bin

    def _print_success(
        self,
        build_time: float,
        firmware_elf: Path,
        firmware_bin: Path,
        bootloader_bin: Optional[Path],
        partitions_bin: Optional[Path],
        merged_bin: Optional[Path],
        size_info: Optional[SizeInfo] = None,
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
            from fbuild.build.build_utils import SizeInfoPrinter

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
            success=False, firmware_bin=None, firmware_elf=None, bootloader_bin=None, partitions_bin=None, merged_bin=None, size_info=None, build_time=time.time() - start_time, message=message
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

        # Strip version constraint (e.g., "platformio/espressif32 @ ^6.12.0" -> "platformio/espressif32")
        spec_base = platform_spec.split("@")[0].strip() if "@" in platform_spec else platform_spec

        # Handle PlatformIO shorthand formats
        if spec_base in ("platformio/espressif32", "espressif32"):
            log_detail(f"Resolving platform shorthand '{platform_spec}' to pioarduino stable release")
            return DEFAULT_ESP32_URL

        # For unknown formats, return as-is and let the download fail with a clear error
        log_warning(f"Unknown platform format: {platform_spec}, attempting to use as URL")
        return platform_spec
