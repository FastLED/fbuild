"""
Build orchestration for Fbuild projects.

This module coordinates the entire build process, from parsing platformio.ini
to generating firmware binaries. It integrates all build system components:
- Configuration parsing (platformio.ini, boards.txt)
- Package management (toolchain, Arduino core)
- Source scanning and preprocessing
- Compilation (avr-gcc/avr-g++)
- Linking (avr-gcc linker, avr-objcopy)
"""

import logging
import time
from pathlib import Path
from typing import TYPE_CHECKING, Optional, List, Any

if TYPE_CHECKING:
    from fbuild.daemon.compilation_queue import CompilationJobQueue

from ..interrupt_utils import handle_keyboard_interrupt_properly
from ..config import PlatformIOConfig, BoardConfig, BoardConfigLoader
from ..config.board_config import BoardConfigError
from ..packages import Cache, Toolchain, ArduinoCore
from ..packages.toolchain import ToolchainError
from ..packages.arduino_core import ArduinoCoreError
from ..packages.library_manager import LibraryError
from .source_scanner import SourceScanner, SourceCollection
from .compiler import CompilerError as CompilerImportError
from .linker import LinkerError as LinkerImportError
from .orchestrator_esp32 import OrchestratorESP32
from .build_utils import SizeInfoPrinter
from .library_dependency_processor import LibraryDependencyProcessor
from .source_compilation_orchestrator import (
    SourceCompilationOrchestrator,
    SourceCompilationOrchestratorError
)
from .build_component_factory import BuildComponentFactory
from .orchestrator import IBuildOrchestrator, BuildResult, BuildOrchestratorError
from .build_state import BuildStateTracker
from .build_info_generator import BuildInfoGenerator
from ..output import (
    log, log_phase, log_detail, log_build_complete, log_firmware_path, set_verbose
)

# Note: Daemon queue access is handled via dynamic import in build method
# to avoid circular dependencies and hard daemon requirement


class BuildOrchestratorAVR(IBuildOrchestrator):
    """
    Orchestrates the complete build process for embedded projects.

    This class coordinates all phases of the build:
    1. Parse platformio.ini configuration
    2. Load board configuration
    3. Ensure toolchain is downloaded and validated
    4. Ensure Arduino core is downloaded and validated
    5. Setup build directories
    6. Download and compile library dependencies
    7. Scan source files (sketch + core + variant)
    8. Compile all sources to object files
    9. Link objects (including libraries) into firmware.elf
    10. Convert to firmware.hex (Intel HEX format)
    11. Display size information

    Example usage:
        orchestrator = BuildOrchestrator()
        result = orchestrator.build(
            project_dir=Path("."),
            env_name="uno",
            clean=False,
            verbose=False
        )
        if result.success:
            print(f"Firmware: {result.hex_path}")
            print(f"Flash: {result.size_info.total_flash} bytes")
    """

    def __init__(
        self,
        cache: Optional[Cache] = None,
        verbose: bool = False
    ):
        """
        Initialize build orchestrator.

        Args:
            cache: Cache instance for package management (optional)
            verbose: Enable verbose output
        """
        self.cache = cache
        self.verbose = verbose

    def _log(self, message: str, verbose_only: bool = True) -> None:
        """
        Log a message and optionally print it.

        Args:
            message: Message to log
            verbose_only: If True, only log if verbose mode is enabled
        """
        if not verbose_only or self.verbose:
            logging.info(message)

    def build(
        self,
        project_dir: Path,
        env_name: Optional[str] = None,
        clean: bool = False,
        verbose: Optional[bool] = None,
        jobs: int | None = None,
        queue: Optional["CompilationJobQueue"] = None,
    ) -> BuildResult:
        """
        Execute complete build process.

        Args:
            project_dir: Project root directory containing platformio.ini
            env_name: Environment name to build (defaults to first/default env)
            clean: Clean build (remove all artifacts before building)
            verbose: Override verbose setting
            jobs: Number of parallel compilation jobs (None = CPU count, 1 = serial)
            queue: Compilation queue from daemon context (injected by build_processor)

        Returns:
            BuildResult with build status and output paths

        Raises:
            BuildOrchestratorError: If build fails at any phase
        """
        start_time = time.time()
        verbose_mode = verbose if verbose is not None else self.verbose
        set_verbose(verbose_mode)

        try:
            project_dir = Path(project_dir).resolve()

            # Initialize cache if not provided
            if self.cache is None:
                self.cache = Cache(project_dir)

            # Phase 1: Parse configuration
            log_phase(1, 9, "Parsing platformio.ini...", verbose_only=not verbose_mode)

            config = self._parse_config(project_dir)

            # Determine environment to build
            if env_name is None:
                env_name = config.get_default_environment()
                if env_name is None:
                    raise BuildOrchestratorError(
                        "No environment specified and no default found in platformio.ini"
                    )

            log_detail(f"Building environment: {env_name}", verbose_only=not verbose_mode)

            env_config = config.get_env_config(env_name)

            # Phase 2: Load board configuration
            log_phase(2, 9, "Loading board configuration...", verbose_only=not verbose_mode)

            board_id = env_config['board']
            board_config = BoardConfigLoader.load_board_config(board_id, env_config)

            log_detail(f"Board: {board_config.name}", verbose_only=not verbose_mode)
            log_detail(f"MCU: {board_config.mcu}", verbose_only=not verbose_mode)
            log_detail(f"F_CPU: {board_config.f_cpu}", verbose_only=not verbose_mode)

            # Detect platform and handle accordingly
            if board_config.platform == "esp32":
                log_detail(f"Platform: {board_config.platform} (using native ESP32 build)", verbose_only=not verbose_mode)
                # Get build flags from platformio.ini
                build_flags = config.get_build_flags(env_name)
                return self._build_esp32(
                    project_dir, env_name, board_id, env_config, clean, verbose_mode, start_time, build_flags, jobs
                )
            elif board_config.platform == "teensy":
                log_detail(f"Platform: {board_config.platform} (using native Teensy build)", verbose_only=not verbose_mode)
                # Get build flags from platformio.ini
                build_flags = config.get_build_flags(env_name)
                return self._build_teensy(
                    project_dir, env_name, board_id, board_config, clean, verbose_mode, start_time, build_flags, jobs
                )
            elif board_config.platform != "avr":
                # Only AVR, ESP32, and Teensy are supported natively
                return BuildResult(
                    success=False,
                    hex_path=None,
                    elf_path=None,
                    size_info=None,
                    build_time=time.time() - start_time,
                    message=f"Platform '{board_config.platform}' is not supported. " +
                           "Fbuild currently supports 'avr', 'esp32', and 'teensy' platforms natively."
                )

            # Phase 3: Ensure toolchain
            log_phase(3, 9, "Ensuring AVR toolchain...", verbose_only=not verbose_mode)

            toolchain = self._ensure_toolchain()

            log_detail("Toolchain ready", verbose_only=not verbose_mode)

            # Phase 4: Ensure Arduino core
            log_phase(4, 9, "Ensuring Arduino core...", verbose_only=not verbose_mode)

            arduino_core = self._ensure_arduino_core()
            core_path = arduino_core.ensure_avr_core()

            log_detail(f"Core ready: version {arduino_core.AVR_VERSION}", verbose_only=not verbose_mode)

            # Phase 5: Setup build directories
            log_phase(5, 11, "Preparing build directories...", verbose_only=not verbose_mode)

            if clean:
                self.cache.clean_build(env_name)

            self.cache.ensure_build_directories(env_name)
            build_dir = self.cache.get_build_dir(env_name)
            core_build_dir = self.cache.get_core_build_dir(env_name)
            src_build_dir = self.cache.get_src_build_dir(env_name)

            # Phase 5.5: Check build state and invalidate cache if needed
            log_phase(5, 11, "Checking build configuration state...", verbose_only=not verbose_mode)

            state_tracker = BuildStateTracker(build_dir)
            build_flags = config.get_build_flags(env_name)
            lib_deps = config.get_lib_deps(env_name)

            needs_rebuild, reasons, current_state = state_tracker.check_invalidation(
                platformio_ini_path=project_dir / "platformio.ini",
                platform=board_config.platform,
                board=board_id,
                framework=env_config.get('framework', 'arduino'),
                toolchain_version=toolchain.VERSION,
                framework_version=arduino_core.AVR_VERSION,
                platform_version=arduino_core.AVR_VERSION,  # Using core version as platform version
                build_flags=build_flags,
                lib_deps=lib_deps,
            )

            if needs_rebuild:
                log_detail("Build cache invalidated:", verbose_only=not verbose_mode)
                for reason in reasons:
                    log_detail(f"  - {reason}", indent=8, verbose_only=not verbose_mode)
                log_detail("Cleaning build artifacts...", verbose_only=not verbose_mode)
                # Clean build artifacts to force rebuild
                self.cache.clean_build(env_name)
                # Recreate directories
                self.cache.ensure_build_directories(env_name)
            else:
                log_detail("Build configuration unchanged, using cached artifacts", verbose_only=not verbose_mode)

            # Phase 6: Download and compile library dependencies
            log_phase(6, 11, "Processing library dependencies...", verbose_only=not verbose_mode)

            lib_deps = config.get_lib_deps(env_name)

            lib_processor = LibraryDependencyProcessor(
                build_dir=build_dir,
                mode="release",
                verbose=verbose_mode
            )

            lib_result = lib_processor.process_dependencies(
                lib_deps=lib_deps,
                toolchain=toolchain,
                board_config=board_config,
                core_path=core_path
            )

            lib_include_paths = lib_result.include_paths
            lib_archives = lib_result.archive_files

            # Get src_dir override from platformio.ini
            from ..config import PlatformIOConfig
            config_for_src_dir = PlatformIOConfig(project_dir / "platformio.ini")
            src_dir_override = config_for_src_dir.get_src_dir()

            # Phase 7: Scan source files
            log_phase(7, 11, "Scanning source files...", verbose_only=not verbose_mode)

            sources = self._scan_sources(
                project_dir,
                build_dir,
                board_config,
                core_path,
                src_dir_override
            )

            total_sources = (
                len(sources.sketch_sources)
                + len(sources.core_sources)
                + len(sources.variant_sources)
            )

            log_detail(f"Sketch: {len(sources.sketch_sources)} files", verbose_only=not verbose_mode)
            log_detail(f"Core: {len(sources.core_sources)} files", verbose_only=not verbose_mode)
            log_detail(f"Variant: {len(sources.variant_sources)} files", verbose_only=not verbose_mode)
            log_detail(f"Total: {total_sources} files", verbose_only=not verbose_mode)

            # Phase 8: Compile sources
            log_phase(8, 11, "Compiling sources...", verbose_only=not verbose_mode)

            # Get compilation queue for this build using context manager
            from fbuild.build.orchestrator import managed_compilation_queue
            with managed_compilation_queue(jobs, verbose_mode, provided_queue=queue) as compilation_queue:
                compiler = BuildComponentFactory.create_compiler(
                    toolchain, board_config, core_path, lib_include_paths, compilation_queue
                )

                compilation_orchestrator = SourceCompilationOrchestrator(verbose=verbose_mode)
                compilation_result = compilation_orchestrator.compile_multiple_groups(
                    compiler=compiler,
                    sketch_sources=sources.sketch_sources,
                    core_sources=sources.core_sources,
                    variant_sources=sources.variant_sources,
                    src_build_dir=src_build_dir,
                    core_build_dir=core_build_dir
                )

                sketch_objects = compilation_result.sketch_objects
                all_core_objects = compilation_result.all_core_objects

                # Phase 9: Link firmware
                log_phase(9, 11, "Linking firmware...", verbose_only=not verbose_mode)

                elf_path = build_dir / 'firmware.elf'
                hex_path = build_dir / 'firmware.hex'

                linker = BuildComponentFactory.create_linker(toolchain, board_config)
                # Now using avr-gcc-ar for archive creation, which properly handles LTO
                # bytecode objects. This allows using archives instead of passing objects directly.
                # Previously, standard ar couldn't create proper symbol indices for LTO objects.
                link_result = linker.link_legacy(
                    sketch_objects,
                    all_core_objects,
                    elf_path,
                    hex_path,
                    lib_archives,  # Library archives (created with gcc-ar for LTO support)
                    None,  # No extra flags
                    None  # No additional objects needed when using archives
                )

                if not link_result.success:
                    raise BuildOrchestratorError(
                        f"Linking failed:\n{link_result.stderr}"
                    )

                log_firmware_path(hex_path, verbose_only=not verbose_mode)

                # Phase 10: Save build state for future cache validation
                log_phase(10, 11, "Saving build state...", verbose_only=not verbose_mode)
                state_tracker.save_state(current_state)

                # Phase 10.5: Generate build_info.json
                build_time = time.time() - start_time
                build_info_generator = BuildInfoGenerator(build_dir)
                toolchain_tools = toolchain.get_all_tools()
                # Parse f_cpu from string (e.g., "16000000L") to int
                f_cpu_int = int(board_config.f_cpu.rstrip("L"))
                # Build toolchain_paths dict, filtering out None values
                toolchain_paths_raw = {
                    "gcc": toolchain_tools.get("avr-gcc"),
                    "gxx": toolchain_tools.get("avr-g++"),
                    "ar": toolchain_tools.get("avr-ar"),
                    "objcopy": toolchain_tools.get("avr-objcopy"),
                    "size": toolchain_tools.get("avr-size"),
                }
                toolchain_paths = {k: v for k, v in toolchain_paths_raw.items() if v is not None}
                build_info = build_info_generator.generate_avr(
                    env_name=env_name,
                    board_id=board_id,
                    board_name=board_config.name,
                    mcu=board_config.mcu,
                    f_cpu=f_cpu_int,
                    build_time=build_time,
                    elf_path=elf_path,
                    hex_path=hex_path,
                    size_info=link_result.size_info,
                    build_flags=build_flags,
                    lib_deps=lib_deps,
                    toolchain_version=toolchain.VERSION,
                    toolchain_paths=toolchain_paths,
                    framework_version=arduino_core.AVR_VERSION,
                    core_path=core_path,
                )
                build_info_generator.save(build_info)
                log_detail(f"Build info saved to {build_info_generator.build_info_path}", verbose_only=not verbose_mode)

                # Phase 11: Display results

                log_phase(11, 11, "Build complete!", verbose_only=not verbose_mode)
                log("")
                SizeInfoPrinter.print_size_info(link_result.size_info)
                log_build_complete(build_time, verbose_only=not verbose_mode)

                return BuildResult(
                    success=True,
                    hex_path=hex_path,
                    elf_path=elf_path,
                    size_info=link_result.size_info,
                    build_time=build_time,
                    message="Build successful"
                )

        except (
            BuildOrchestratorError,
            ToolchainError,
            ArduinoCoreError,
            CompilerImportError,
            LinkerImportError,
            BoardConfigError,
            LibraryError,
            SourceCompilationOrchestratorError
        ) as e:
            build_time = time.time() - start_time
            return BuildResult(
                success=False,
                hex_path=None,
                elf_path=None,
                size_info=None,
                build_time=build_time,
                message=str(e)
            )
        except KeyboardInterrupt as ke:
            handle_keyboard_interrupt_properly(ke)
        except Exception as e:
            build_time = time.time() - start_time
            return BuildResult(
                success=False,
                hex_path=None,
                elf_path=None,
                size_info=None,
                build_time=build_time,
                message=f"Unexpected error: {e}"
            )

    def _build_esp32(
        self,
        project_dir: Path,
        env_name: str,
        board_id: str,
        env_config: dict[str, Any],
        clean: bool,
        verbose: bool,
        start_time: float,
        build_flags: List[str],
        jobs: int | None = None
    ) -> BuildResult:
        """
        Build ESP32 project using native build system.

        Delegates to ESP32Orchestrator for ESP32-specific build logic.

        Args:
            project_dir: Project directory
            env_name: Environment name
            board_id: Board ID (e.g., esp32-c6-devkitm-1)
            env_config: Environment configuration dict
            clean: Whether to clean before build
            verbose: Verbose output
            start_time: Build start time
            build_flags: User build flags from platformio.ini
            jobs: Number of parallel compilation jobs (None = CPU count, 1 = serial)

        Returns:
            BuildResult
        """
        if self.cache is None:
            return BuildResult(
                success=False,
                hex_path=None,
                elf_path=None,
                size_info=None,
                build_time=time.time() - start_time,
                message="Cache is required for ESP32 builds"
            )

        esp32_orchestrator = OrchestratorESP32(self.cache, verbose)
        # Use the new BaseBuildOrchestrator-compliant interface
        result = esp32_orchestrator.build(
            project_dir=project_dir,
            env_name=env_name,
            clean=clean,
            verbose=verbose,
            jobs=jobs
        )
        return result

    def _build_teensy(
        self,
        project_dir: Path,
        env_name: str,
        board_id: str,
        board_config: BoardConfig,
        clean: bool,
        verbose: bool,
        start_time: float,
        build_flags: List[str],
        jobs: int | None = None
    ) -> BuildResult:
        """
        Build Teensy project using native build system.

        Args:
            project_dir: Project directory
            env_name: Environment name
            board_id: Board ID (e.g., teensy41)
            board_config: Board configuration
            clean: Whether to clean before build
            verbose: Verbose output
            start_time: Build start time
            build_flags: User build flags from platformio.ini
            jobs: Number of parallel compilation jobs (None = CPU count, 1 = serial)

        Returns:
            BuildResult
        """
        if self.cache is None:
            return BuildResult(
                success=False,
                hex_path=None,
                elf_path=None,
                size_info=None,
                build_time=time.time() - start_time,
                message="Cache not initialized"
            )

        # Delegate to OrchestratorTeensy for native Teensy build
        from .orchestrator_teensy import OrchestratorTeensy

        teensy_orchestrator = OrchestratorTeensy(self.cache, verbose)
        result = teensy_orchestrator.build(
            project_dir=project_dir,
            env_name=env_name,
            clean=clean,
            verbose=verbose,
            jobs=jobs
        )

        return result

    def _parse_config(self, project_dir: Path) -> PlatformIOConfig:
        """
        Parse platformio.ini configuration file.

        Args:
            project_dir: Project directory

        Returns:
            PlatformIOConfig instance

        Raises:
            BuildOrchestratorError: If platformio.ini not found or invalid
        """
        ini_path = project_dir / 'platformio.ini'

        if not ini_path.exists():
            raise BuildOrchestratorError(
                f"platformio.ini not found in {project_dir}\n" +
                "Make sure you're in a valid project directory."
            )

        try:
            return PlatformIOConfig(ini_path)
        except KeyboardInterrupt as ke:
            handle_keyboard_interrupt_properly(ke)
        except Exception as e:
            raise BuildOrchestratorError(
                f"Failed to parse platformio.ini: {e}"
            )

    def _ensure_toolchain(self) -> Toolchain:
        """
        Ensure AVR toolchain is available.

        Returns:
            Toolchain instance with toolchain ready

        Raises:
            BuildOrchestratorError: If toolchain cannot be obtained
        """
        try:
            cache = self.cache if self.cache else Cache()
            toolchain = Toolchain(cache)
            toolchain.ensure_toolchain()
            return toolchain
        except KeyboardInterrupt as ke:
            handle_keyboard_interrupt_properly(ke)
        except Exception as e:
            raise BuildOrchestratorError(
                f"Failed to setup toolchain: {e}"
            )

    def _ensure_arduino_core(self) -> ArduinoCore:
        """
        Ensure Arduino core is available.

        Returns:
            ArduinoCore instance with core ready

        Raises:
            BuildOrchestratorError: If core cannot be obtained
        """
        try:
            cache = self.cache if self.cache else Cache()
            arduino_core = ArduinoCore(cache)
            arduino_core.ensure_avr_core()
            return arduino_core
        except KeyboardInterrupt as ke:
            handle_keyboard_interrupt_properly(ke)
        except Exception as e:
            raise BuildOrchestratorError(
                f"Failed to setup Arduino core: {e}"
            )

    def _scan_sources(
        self,
        project_dir: Path,
        build_dir: Path,
        board_config: BoardConfig,
        core_path: Path,
        src_dir_override: Optional[str] = None
    ) -> "SourceCollection":
        """
        Scan for all source files.

        Args:
            project_dir: Project directory
            build_dir: Build output directory
            board_config: Board configuration
            core_path: Arduino core installation path
            src_dir_override: Optional source directory override (relative to project_dir)

        Returns:
            SourceCollection with all sources
        """
        scanner = SourceScanner(project_dir, build_dir)

        # Determine source directories
        # Use src_dir override from platformio.ini if specified
        if src_dir_override:
            src_dir = project_dir / src_dir_override
        else:
            # Check if 'src' directory exists, otherwise use project root
            src_dir = project_dir / 'src'
            if not src_dir.exists():
                src_dir = project_dir

        core_dir = board_config.get_core_sources_dir(core_path)
        variant_dir = board_config.get_variant_dir(core_path)

        return scanner.scan(
            src_dir=src_dir,
            core_dir=core_dir,
            variant_dir=variant_dir
        )

