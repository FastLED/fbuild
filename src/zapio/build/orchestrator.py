"""
Build orchestration for Zapio projects.

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
from typing import Optional
from dataclasses import dataclass

from ..config import PlatformIOConfig, BoardConfig
from ..config.board_config import BoardConfigError
from ..packages import Cache, Toolchain, ArduinoCore
from ..packages.toolchain import ToolchainError
from ..packages.arduino_core import ArduinoCoreError
from ..packages.library_manager import LibraryManager, LibraryError
from ..packages.esp32_platform import ESP32Platform
from ..packages.esp32_toolchain import ESP32Toolchain
from ..packages.esp32_framework import ESP32Framework
from ..packages.esp32_library_manager import ESP32LibraryManager
from .source_scanner import SourceScanner
from .compiler import Compiler
from .compiler import CompilerError as CompilerImportError
from .esp32_compiler import ESP32Compiler
from .linker import Linker, SizeInfo
from .linker import LinkerError as LinkerImportError
from .esp32_linker import ESP32Linker


@dataclass
class BuildResult:
    """Result of a complete build operation."""

    success: bool
    hex_path: Optional[Path]
    elf_path: Optional[Path]
    size_info: Optional[SizeInfo]
    build_time: float
    message: str


class BuildOrchestratorError(Exception):
    """Exception raised for build orchestration errors."""
    pass


class BuildOrchestrator:
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
            board_config = self._load_board_config(board_id, env_config)

            if verbose_mode:
                print(f"      Board: {board_config.name}")
                print(f"      MCU: {board_config.mcu}")
                print(f"      F_CPU: {board_config.f_cpu}")

            # Detect platform and handle accordingly
            if board_config.platform == "esp32":
                if verbose_mode:
                    print(f"      Platform: {board_config.platform} (using native ESP32 build)")
                return self._build_esp32_native(
                    project_dir, env_name, board_id, env_config, clean, verbose_mode, start_time
                )
            elif board_config.platform != "avr":
                # Only AVR and ESP32 are supported natively
                return BuildResult(
                    success=False,
                    hex_path=None,
                    elf_path=None,
                    size_info=None,
                    build_time=time.time() - start_time,
                    message=f"Platform '{board_config.platform}' is not supported. "
                           f"Zapio currently supports 'avr' and 'esp32' platforms natively."
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

            # Phase 6: Download and compile library dependencies
            if verbose_mode:
                print("[6/11] Processing library dependencies...")

            lib_deps = config.get_lib_deps(env_name)
            libraries = []
            lib_include_paths = []
            lib_objects = []

            if lib_deps:
                if verbose_mode:
                    print(f"      Found {len(lib_deps)} library dependencies")

                library_manager = LibraryManager(build_dir, mode="release")

                # Get compiler info for library compilation
                tools = toolchain.get_all_tools()
                lib_defines_dict = board_config.get_defines()
                lib_includes = board_config.get_include_paths(core_path)

                # Convert defines dict to list format for library_manager
                lib_defines = []
                for key, value in lib_defines_dict.items():
                    if value:
                        lib_defines.append(f"{key}={value}")
                    else:
                        lib_defines.append(key)

                libraries = library_manager.ensure_libraries(
                    lib_deps=lib_deps,
                    compiler_path=tools['avr-gcc'],
                    mcu=board_config.mcu,
                    f_cpu=board_config.f_cpu,
                    defines=lib_defines,
                    include_paths=lib_includes,
                    extra_flags=[],
                    show_progress=verbose_mode
                )

                lib_include_paths = library_manager.get_library_include_paths()
                lib_objects = library_manager.get_library_objects()

                if verbose_mode:
                    print(f"      Compiled {len(libraries)} libraries")
                    print(f"      Library objects: {len(lib_objects)}")
            else:
                if verbose_mode:
                    print("      No library dependencies")

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

            compiler = self._create_compiler(
                toolchain, board_config, core_path, lib_include_paths
            )

            # Compile sketch sources
            sketch_objects = self._compile_sources(
                compiler,
                sources.sketch_sources,
                src_build_dir,
                verbose_mode,
                "sketch"
            )

            # Compile core sources
            core_objects = self._compile_sources(
                compiler,
                sources.core_sources,
                core_build_dir,
                verbose_mode,
                "core"
            )

            # Compile variant sources
            variant_objects = self._compile_sources(
                compiler,
                sources.variant_sources,
                core_build_dir,
                verbose_mode,
                "variant"
            )

            # Combine core and variant objects
            all_core_objects = core_objects + variant_objects

            if verbose_mode:
                print(f"      Compiled {len(sketch_objects) + len(all_core_objects)} objects")

            # Phase 9: Link firmware
            if verbose_mode:
                print("[9/11] Linking firmware...")

            elf_path = build_dir / 'firmware.elf'
            hex_path = build_dir / 'firmware.hex'

            linker = self._create_linker(toolchain, board_config)
            # For LTO with -fno-fat-lto-objects, we pass library objects separately
            # so they don't get archived (LTO bytecode doesn't work well in archives)
            link_result = linker.link(
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

            # Phase 10-11: Display results
            build_time = time.time() - start_time

            if verbose_mode:
                print("[10-11/11] Build complete!")
                print()
                self._print_size_info(link_result.size_info)
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
            LibraryError
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

    def _build_esp32_native(
        self,
        project_dir: Path,
        env_name: str,
        board_id: str,
        env_config: dict,
        clean: bool,
        verbose: bool,
        start_time: float
    ) -> BuildResult:
        """
        Build ESP32 project using native build system (without PlatformIO).

        Args:
            project_dir: Project directory
            env_name: Environment name
            board_id: Board ID (e.g., esp32-c6-devkitm-1)
            env_config: Environment configuration dict
            clean: Whether to clean before build
            verbose: Verbose output
            start_time: Build start time

        Returns:
            BuildResult
        """
        try:
            # Ensure cache is available
            if self.cache is None:
                return BuildResult(
                    success=False,
                    hex_path=None,
                    elf_path=None,
                    size_info=None,
                    build_time=time.time() - start_time,
                    message="Cache is required for ESP32 builds"
                )

            # Get platform URL from env_config
            platform_url = env_config.get('platform')
            if not platform_url:
                return BuildResult(
                    success=False,
                    hex_path=None,
                    elf_path=None,
                    size_info=None,
                    build_time=time.time() - start_time,
                    message="No platform URL specified in platformio.ini"
                )

            if verbose:
                print("[3/10] Initializing ESP32 platform...")

            # Initialize platform
            platform = ESP32Platform(self.cache, platform_url, show_progress=verbose)
            platform.ensure_platform()

            # Get board configuration
            board_json = platform.get_board_json(board_id)
            mcu = board_json.get("build", {}).get("mcu", "esp32c6")

            if verbose:
                print(f"      Board: {board_id}")
                print(f"      MCU: {mcu}")

            # Get required packages
            packages = platform.get_required_packages(mcu)

            # Initialize toolchain
            if verbose:
                print("[4/10] Initializing ESP32 toolchain...")

            toolchain_url = packages.get("toolchain-riscv32-esp") or packages.get("toolchain-xtensa-esp-elf")
            if not toolchain_url:
                return BuildResult(
                    success=False,
                    hex_path=None,
                    elf_path=None,
                    size_info=None,
                    build_time=time.time() - start_time,
                    message="Toolchain URL not found in platform package"
                )

            # Determine toolchain type
            toolchain_type = "riscv32-esp" if "riscv32" in toolchain_url else "xtensa-esp-elf"
            toolchain = ESP32Toolchain(
                self.cache,
                toolchain_url,
                toolchain_type,
                show_progress=verbose
            )
            toolchain.ensure_toolchain()

            # Initialize framework
            if verbose:
                print("[5/10] Initializing ESP32 framework...")

            framework_url = packages.get("framework-arduinoespressif32")
            libs_url = packages.get("framework-arduinoespressif32-libs")

            if not framework_url or not libs_url:
                return BuildResult(
                    success=False,
                    hex_path=None,
                    elf_path=None,
                    size_info=None,
                    build_time=time.time() - start_time,
                    message="Framework URLs not found in platform package"
                )

            framework = ESP32Framework(
                self.cache,
                framework_url,
                libs_url,
                show_progress=verbose
            )
            framework.ensure_framework()

            # Setup build directory
            build_dir = self.cache.get_build_dir(env_name)

            if clean and build_dir.exists():
                if verbose:
                    print("[6/10] Cleaning build directory...")
                import shutil
                shutil.rmtree(build_dir)

            build_dir.mkdir(parents=True, exist_ok=True)

            # Initialize compiler
            if verbose:
                print("[7/10] Compiling Arduino core...")

            compiler = ESP32Compiler(
                platform,
                toolchain,
                framework,
                board_id,
                build_dir,
                show_progress=verbose
            )

            # Compile Arduino core
            core_obj_files = compiler.compile_core()
            core_archive = compiler.create_core_archive(core_obj_files)

            if verbose:
                print(f"      Compiled {len(core_obj_files)} core source files")

            # Handle library dependencies
            lib_deps = env_config.get('lib_deps', '')
            library_archives = []
            library_include_paths = []

            if lib_deps:
                if verbose:
                    print("[7.5/10] Processing library dependencies...")

                # Parse lib_deps (can be string or list)
                if isinstance(lib_deps, str):
                    lib_specs = [dep.strip() for dep in lib_deps.split('\n') if dep.strip()]
                else:
                    lib_specs = lib_deps

                if lib_specs:
                    # Initialize library manager
                    lib_manager = ESP32LibraryManager(build_dir)

                    # Get compiler flags for library compilation
                    lib_compiler_flags = compiler.get_base_flags()

                    # Get include paths for library compilation
                    lib_include_paths = compiler.get_include_paths()

                    # Get toolchain bin path
                    toolchain_bin_path = toolchain.get_bin_path()
                    if toolchain_bin_path is None:
                        return BuildResult(
                            success=False,
                            hex_path=None,
                            elf_path=None,
                            size_info=None,
                            build_time=time.time() - start_time,
                            message="Toolchain bin directory not found"
                        )

                    # Ensure libraries are downloaded and compiled
                    libraries = lib_manager.ensure_libraries(
                        lib_specs,
                        toolchain_bin_path,
                        lib_compiler_flags,
                        lib_include_paths,
                        show_progress=verbose
                    )

                    # Get library archives and include paths
                    library_archives = [lib.archive_file for lib in libraries if lib.is_compiled]
                    library_include_paths = lib_manager.get_library_include_paths()

                    if verbose:
                        print(f"      Compiled {len(libraries)} library dependencies")

            # Add library include paths to compiler
            if library_include_paths:
                compiler.add_library_includes(library_include_paths)

            # Find and compile sketch
            if verbose:
                print("[8/10] Compiling sketch...")

            # Look for .ino files in the project directory
            sketch_files = list(project_dir.glob("*.ino"))
            if not sketch_files:
                return BuildResult(
                    success=False,
                    hex_path=None,
                    elf_path=None,
                    size_info=None,
                    build_time=time.time() - start_time,
                    message=f"No .ino sketch file found in {project_dir}"
                )

            sketch_path = sketch_files[0]
            sketch_obj_files = compiler.compile_sketch(sketch_path)

            if verbose:
                print(f"      Compiled {len(sketch_obj_files)} sketch file(s)")

            # Initialize linker
            if verbose:
                print("[9/10] Linking firmware...")

            linker = ESP32Linker(
                platform,
                toolchain,
                framework,
                board_id,
                build_dir,
                show_progress=verbose
            )

            # Link firmware
            firmware_elf = linker.link(sketch_obj_files, core_archive, library_archives=library_archives)

            # Generate binary
            if verbose:
                print("[10/10] Generating firmware binary...")

            firmware_bin = linker.generate_bin(firmware_elf)

            build_time = time.time() - start_time

            if verbose:
                print()
                print("=" * 60)
                print("BUILD SUCCESSFUL!")
                print("=" * 60)
                print(f"  Build time: {build_time:.2f}s")
                print(f"  Firmware ELF: {firmware_elf}")
                print(f"  Firmware BIN: {firmware_bin}")
                print()

            return BuildResult(
                success=True,
                hex_path=firmware_bin,  # Use .bin as hex_path for ESP32
                elf_path=firmware_elf,
                size_info=None,  # TODO: Add size info for ESP32
                build_time=build_time,
                message="Build successful (native ESP32 build)"
            )

        except Exception as e:
            build_time = time.time() - start_time
            import traceback
            error_trace = traceback.format_exc()
            return BuildResult(
                success=False,
                hex_path=None,
                elf_path=None,
                size_info=None,
                build_time=build_time,
                message=f"ESP32 native build failed: {e}\n\n{error_trace}"
            )

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
                f"platformio.ini not found in {project_dir}\n"
                "Make sure you're in a valid project directory."
            )

        try:
            return PlatformIOConfig(ini_path)
        except Exception as e:
            raise BuildOrchestratorError(
                f"Failed to parse platformio.ini: {e}"
            )

    def _load_board_config(
        self,
        board_id: str,
        env_config: dict
    ) -> BoardConfig:
        """
        Load board configuration.

        Args:
            board_id: Board identifier (e.g., 'uno')
            env_config: Environment configuration from platformio.ini

        Returns:
            BoardConfig instance

        Raises:
            BuildOrchestratorError: If board configuration cannot be loaded
        """
        # Extract overrides from env_config (board_build.*)
        overrides = {}
        for key, value in env_config.items():
            if key.startswith('board_build.'):
                override_key = key[12:]  # Remove 'board_build.' prefix
                overrides[override_key] = value

        try:
            # Try to load from built-in defaults first
            return BoardConfig.from_board_id(board_id, overrides)
        except BoardConfigError:
            # Check if this is an ESP32 board by looking at platform URL
            platform_url = env_config.get('platform', '')
            if 'espressif32' in platform_url or 'esp32' in platform_url.lower():
                # For ESP32 boards, create a minimal BoardConfig
                # The actual board JSON will be loaded by ESP32Platform
                # Infer MCU from board_id (e.g., esp32dev -> esp32, esp32-s3-* -> esp32s3)
                mcu = "esp32"
                if "esp32s3" in board_id or "esp32-s3" in board_id:
                    mcu = "esp32s3"
                elif "esp32s2" in board_id or "esp32-s2" in board_id:
                    mcu = "esp32s2"
                elif "esp32c3" in board_id or "esp32-c3" in board_id:
                    mcu = "esp32c3"
                elif "esp32c6" in board_id or "esp32-c6" in board_id:
                    mcu = "esp32c6"
                elif "esp32h2" in board_id or "esp32-h2" in board_id:
                    mcu = "esp32h2"

                return BoardConfig(
                    name=board_id,
                    mcu=mcu,
                    f_cpu="240000000L",  # Default ESP32 frequency
                    board="ESP32_DEV",
                    core="esp32",
                    variant="esp32"
                )

            # If not ESP32, re-raise original error
            raise BuildOrchestratorError(
                f"Unknown board: {board_id}\n"
                "Supported AVR boards: uno, mega, nano, leonardo\n"
                "For ESP32 boards, ensure platform URL contains 'espressif32'"
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
    ):
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

    def _create_compiler(
        self,
        toolchain: Toolchain,
        board_config: BoardConfig,
        core_path: Path,
        lib_include_paths: list[Path] | None = None
    ) -> Compiler:
        """
        Create compiler instance with appropriate settings.

        Args:
            toolchain: Toolchain instance
            board_config: Board configuration
            core_path: Arduino core path
            lib_include_paths: Optional library include paths

        Returns:
            Configured Compiler instance
        """
        tools = toolchain.get_all_tools()
        include_paths = board_config.get_include_paths(core_path)

        # Add library include paths
        if lib_include_paths:
            include_paths = list(include_paths) + list(lib_include_paths)

        defines = board_config.get_defines()

        return Compiler(
            avr_gcc=tools['avr-gcc'],
            avr_gpp=tools['avr-g++'],
            mcu=board_config.mcu,
            f_cpu=board_config.f_cpu,
            includes=include_paths,
            defines=defines
        )

    def _create_linker(
        self,
        toolchain: Toolchain,
        board_config: BoardConfig
    ) -> Linker:
        """
        Create linker instance with appropriate settings.

        Args:
            toolchain: Toolchain instance
            board_config: Board configuration

        Returns:
            Configured Linker instance
        """
        tools = toolchain.get_all_tools()

        # Determine max flash/RAM for the board
        # Arduino Uno: 32KB flash, 2KB RAM
        max_flash = {
            'atmega328p': 32256,   # 32KB - 512B bootloader
            'atmega2560': 253952,  # 256KB - 8KB bootloader
            'atmega32u4': 28672,   # 32KB - 4KB bootloader
        }.get(board_config.mcu, None)

        max_ram = {
            'atmega328p': 2048,    # 2KB
            'atmega2560': 8192,    # 8KB
            'atmega32u4': 2560,    # 2.5KB
        }.get(board_config.mcu, None)

        return Linker(
            avr_gcc=tools['avr-gcc'],
            avr_ar=tools['avr-ar'],
            avr_objcopy=tools['avr-objcopy'],
            avr_size=tools['avr-size'],
            mcu=board_config.mcu,
            max_flash=max_flash,
            max_ram=max_ram
        )

    def _compile_sources(
        self,
        compiler: Compiler,
        sources: list[Path],
        output_dir: Path,
        verbose: bool,
        source_type: str
    ) -> list[Path]:
        """
        Compile list of source files.

        Args:
            compiler: Compiler instance
            sources: List of source files
            output_dir: Output directory for object files
            verbose: Verbose output
            source_type: Type of sources (for logging)

        Returns:
            List of compiled object file paths

        Raises:
            BuildOrchestratorError: If compilation fails
        """
        output_dir.mkdir(parents=True, exist_ok=True)
        objects = []

        for source in sources:
            # Generate output object filename
            obj_name = source.stem + '.o'
            obj_path = output_dir / obj_name

            # Check if rebuild needed
            if not compiler.needs_rebuild(source, obj_path):
                if verbose:
                    print(f"      [{source_type}] {source.name} (cached)")
                objects.append(obj_path)
                continue

            # Compile
            if verbose:
                print(f"      [{source_type}] {source.name}")

            result = compiler.compile(source, obj_path)

            if not result.success:
                raise BuildOrchestratorError(
                    f"Compilation failed for {source}:\n{result.stderr}"
                )

            objects.append(obj_path)

        return objects

    def _print_size_info(self, size_info: Optional[SizeInfo]) -> None:
        """
        Print firmware size information.

        Args:
            size_info: Size information from linker
        """
        if not size_info:
            return

        print("Firmware Size:")
        print(f"  Program:  {size_info.total_flash:6d} bytes", end="")
        if size_info.flash_percent is not None:
            print(f" ({size_info.flash_percent:5.1f}% of {size_info.max_flash} bytes)")
        else:
            print()

        print(f"  Data:     {size_info.data:6d} bytes")
        print(f"  BSS:      {size_info.bss:6d} bytes")
        print(f"  RAM:      {size_info.total_ram:6d} bytes", end="")
        if size_info.ram_percent is not None:
            print(f" ({size_info.ram_percent:5.1f}% of {size_info.max_ram} bytes)")
        else:
            print()
