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

import time
from pathlib import Path
from typing import Optional, List, Any

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

    def build(
        self,
        project_dir: Path,
        env_name: Optional[str] = None,
        clean: bool = False,
        verbose: Optional[bool] = None
    ) -> BuildResult:
        """
        Execute complete build process.

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
        start_time = time.time()
        verbose_mode = verbose if verbose is not None else self.verbose

        try:
            project_dir = Path(project_dir).resolve()

            # Initialize cache if not provided
            if self.cache is None:
                self.cache = Cache(project_dir)

            # Phase 1: Parse configuration
            if verbose_mode:
                print("[1/9] Parsing platformio.ini...")

            config = self._parse_config(project_dir)

            # Determine environment to build
            if env_name is None:
                env_name = config.get_default_environment()
                if env_name is None:
                    raise BuildOrchestratorError(
                        "No environment specified and no default found in platformio.ini"
                    )

            if verbose_mode:
                print(f"      Building environment: {env_name}")

            env_config = config.get_env_config(env_name)

            # Phase 2: Load board configuration
            if verbose_mode:
                print("[2/9] Loading board configuration...")

            board_id = env_config['board']
            board_config = BoardConfigLoader.load_board_config(board_id, env_config)

            if verbose_mode:
                print(f"      Board: {board_config.name}")
                print(f"      MCU: {board_config.mcu}")
                print(f"      F_CPU: {board_config.f_cpu}")

            # Detect platform and handle accordingly
            if board_config.platform == "esp32":
                if verbose_mode:
                    print(f"      Platform: {board_config.platform} (using native ESP32 build)")
                # Get build flags from platformio.ini
                build_flags = config.get_build_flags(env_name)
                return self._build_esp32(
                    project_dir, env_name, board_id, env_config, clean, verbose_mode, start_time, build_flags
                )
            elif board_config.platform == "teensy":
                if verbose_mode:
                    print(f"      Platform: {board_config.platform} (using native Teensy build)")
                # Get build flags from platformio.ini
                build_flags = config.get_build_flags(env_name)
                return self._build_teensy(
                    project_dir, env_name, board_id, board_config, clean, verbose_mode, start_time, build_flags
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
            if verbose_mode:
                print("[3/9] Ensuring AVR toolchain...")

            toolchain = self._ensure_toolchain()

            if verbose_mode:
                print("      Toolchain ready")

            # Phase 4: Ensure Arduino core
            if verbose_mode:
                print("[4/9] Ensuring Arduino core...")

            arduino_core = self._ensure_arduino_core()
            core_path = arduino_core.ensure_avr_core()

            if verbose_mode:
                print(f"      Core ready: version {arduino_core.AVR_VERSION}")

            # Phase 5: Setup build directories
            if verbose_mode:
                print("[5/11] Preparing build directories...")

            if clean:
                self.cache.clean_build(env_name)

            self.cache.ensure_build_directories(env_name)
            build_dir = self.cache.get_build_dir(env_name)
            core_build_dir = self.cache.get_core_build_dir(env_name)
            src_build_dir = self.cache.get_src_build_dir(env_name)

            # Phase 5.5: Check build state and invalidate cache if needed
            if verbose_mode:
                print("[5.5/11] Checking build configuration state...")

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
                if verbose_mode:
                    print("      Build cache invalidated:")
                    for reason in reasons:
                        print(f"        - {reason}")
                    print("      Cleaning build artifacts...")
                # Clean build artifacts to force rebuild
                self.cache.clean_build(env_name)
                # Recreate directories
                self.cache.ensure_build_directories(env_name)
            else:
                if verbose_mode:
                    print("      Build configuration unchanged, using cached artifacts")

            # Phase 6: Download and compile library dependencies
            if verbose_mode:
                print("[6/11] Processing library dependencies...")

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
            lib_objects = lib_result.object_files

            # Phase 7: Scan source files
            if verbose_mode:
                print("[7/11] Scanning source files...")

            sources = self._scan_sources(
                project_dir,
                build_dir,
                board_config,
                core_path
            )

            total_sources = (
                len(sources.sketch_sources)
                + len(sources.core_sources)
                + len(sources.variant_sources)
            )

            if verbose_mode:
                print(f"      Sketch: {len(sources.sketch_sources)} files")
                print(f"      Core: {len(sources.core_sources)} files")
                print(f"      Variant: {len(sources.variant_sources)} files")
                print(f"      Total: {total_sources} files")

            # Phase 8: Compile sources
            if verbose_mode:
                print("[8/11] Compiling sources...")

            compiler = BuildComponentFactory.create_compiler(
                toolchain, board_config, core_path, lib_include_paths
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
            if verbose_mode:
                print("[9/11] Linking firmware...")

            elf_path = build_dir / 'firmware.elf'
            hex_path = build_dir / 'firmware.hex'

            linker = BuildComponentFactory.create_linker(toolchain, board_config)
            # For LTO with -fno-fat-lto-objects, we pass library objects separately
            # so they don't get archived (LTO bytecode doesn't work well in archives)
            link_result = linker.link_legacy(
                sketch_objects,
                all_core_objects,
                elf_path,
                hex_path,
                [],  # No library archives
                None,  # No extra flags
                lib_objects  # Library objects passed separately for LTO
            )

            if not link_result.success:
                raise BuildOrchestratorError(
                    f"Linking failed:\n{link_result.stderr}"
                )

            if verbose_mode:
                print(f"      Firmware: {hex_path}")

            # Phase 10: Save build state for future cache validation
            if verbose_mode:
                print("[10/11] Saving build state...")
            state_tracker.save_state(current_state)

            # Phase 11: Display results
            build_time = time.time() - start_time

            if verbose_mode:
                print("[11/11] Build complete!")
                print()
                SizeInfoPrinter.print_size_info(link_result.size_info)
                print()
                print(f"Build time: {build_time:.2f}s")

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
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
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
        build_flags: List[str]
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
            verbose=verbose
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
        build_flags: List[str]
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
            verbose=verbose
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
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
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
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
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
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise BuildOrchestratorError(
                f"Failed to setup Arduino core: {e}"
            )

    def _scan_sources(
        self,
        project_dir: Path,
        build_dir: Path,
        board_config: BoardConfig,
        core_path: Path
    ) -> "SourceCollection":
        """
        Scan for all source files.

        Args:
            project_dir: Project directory
            build_dir: Build output directory
            board_config: Board configuration
            core_path: Arduino core installation path

        Returns:
            SourceCollection with all sources
        """
        scanner = SourceScanner(project_dir, build_dir)

        # Determine source directories
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

