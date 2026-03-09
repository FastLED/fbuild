"""
Teensy-specific build orchestration for Fbuild projects.

This module handles Teensy platform builds separately from AVR/ESP32 builds,
providing cleaner separation of concerns and better maintainability.
"""

import _thread
import time
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, List, Optional

if TYPE_CHECKING:
    from fbuild.build.build_context import BuildParams

from fbuild.build.build_info_generator import BuildInfoGenerator
from fbuild.build.build_state import BuildStateTracker
from fbuild.build.build_utils import safe_rmtree
from fbuild.build.configurable_compiler import ConfigurableCompiler
from fbuild.build.configurable_linker import ConfigurableLinker
from fbuild.build.linker import SizeInfo
from fbuild.build.orchestrator import BuildResult, IBuildOrchestrator
from fbuild.cli_utils import BannerFormatter
from fbuild.config.board_config import BoardConfig
from fbuild.packages import Cache
from fbuild.packages.library_manager import LibraryError, LibraryManager
from fbuild.packages.platform_teensy import PlatformTeensy
from fbuild.packages.toolchain_teensy import ToolchainTeensy


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
            board_id = env_config.get("board", "teensy41")
            build_flags = config.get_build_flags(env_name)
            build_src_flags = config.get_build_src_flags(env_name)
            lib_deps = config.get_lib_deps(env_name)

            # Call internal build method
            teensy_result = self._build_teensy(board_id, env_config, build_flags, build_src_flags, lib_deps, request)

            # Convert BuildResultTeensy to BuildResult
            return BuildResult(
                success=teensy_result.success,
                hex_path=teensy_result.firmware_hex,
                elf_path=teensy_result.firmware_elf,
                size_info=teensy_result.size_info,
                build_time=teensy_result.build_time,
                message=teensy_result.message,
            )

        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception as e:
            return BuildResult(success=False, hex_path=None, elf_path=None, size_info=None, build_time=0.0, message=f"Failed to parse configuration: {e}")

    def _build_teensy(
        self,
        board_id: str,
        env_config: dict,
        build_flags: List[str],
        build_src_flags: List[str],
        lib_deps: List[str],
        request: "BuildParams",
    ) -> BuildResultTeensy:
        """
        Execute complete Teensy build process (internal implementation).

        Args:
            board_id: Board ID (e.g., teensy41)
            env_config: Environment configuration dict
            build_flags: User build flags from platformio.ini (global)
            build_src_flags: User build_src_flags from platformio.ini (sketch-only)
            lib_deps: Library dependencies from platformio.ini
            request: Build request with basic parameters

        Returns:
            BuildResultTeensy with build status and output paths
        """
        start_time = time.time()

        # Extract from request
        project_dir = request.project_dir
        env_name = request.env_name
        verbose = request.verbose
        build_dir = request.build_dir

        try:
            # Get board configuration
            from fbuild.config.board_config import BoardConfig

            if verbose:
                print("[2/7] Loading board configuration...")

            board_config = BoardConfig.from_board_id(board_id)

            # Print build profile banner
            from fbuild.build.build_profiles import print_profile_banner

            print_profile_banner(request.profile)

            # Initialize platform
            if verbose:
                print("[3/7] Initializing Teensy platform...")

            platform = PlatformTeensy(self.cache, board_config.mcu, show_progress=verbose)
            platform.ensure_package()

            if verbose:
                print(f"      Board: {board_id}")
                print(f"      MCU: {board_config.mcu}")
                print(f"      CPU Frequency: {board_config.f_cpu}")

            # Ensure build directory exists
            build_dir.mkdir(parents=True, exist_ok=True)

            # Check build state and invalidate cache if needed
            if verbose:
                print("[3.5/7] Checking build configuration state...")

            state_tracker = BuildStateTracker(build_dir)
            needs_rebuild, reasons, current_state = state_tracker.check_invalidation(
                platformio_ini_path=project_dir / "platformio.ini",
                platform="teensy",
                board=board_id,
                framework=env_config.get("framework", "arduino"),
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

            # Initialize compilation executor
            from fbuild.build.compilation_executor import CompilationExecutor

            compilation_executor = CompilationExecutor(
                build_dir=build_dir,
                show_progress=verbose,
                compile_database=request.compile_database,
                execute_compilations=not request.generate_compiledb,
            )

            # Load board JSON and platform config ONCE (not redundantly in compiler/linker)
            board_json = platform.get_board_json(board_id)
            from fbuild import platform_configs

            # Load board-specific config (teensy41.json) instead of MCU config (imxrt1062.json)
            # Board configs have board-specific defines like ARDUINO_TEENSY41
            platform_config = platform_configs.load_board_config(board_id)
            if platform_config is None:
                return self._error_result(start_time, f"No platform configuration found for {board_id}. Available: {platform_configs.list_available_configs()}")

            # Extract variant and core from board config
            variant = board_json.get("build", {}).get("variant", "")
            core = board_json.get("build", {}).get("core", "arduino")

            # Create full BuildContext with all configuration loaded once
            from fbuild.build.build_context import BuildContext

            context = BuildContext.from_request(
                request=request,
                platform=platform,
                toolchain=platform.toolchain,
                mcu=board_config.mcu,
                framework_version=platform.framework.version,
                compilation_executor=compilation_executor,
                cache=self.cache,
                # New consolidated fields
                framework=platform.framework,
                board_id=board_id,
                board_config=board_json,
                platform_config=platform_config,
                variant=variant,
                core=core,
                user_build_flags=build_flags,
                user_build_src_flags=build_src_flags,
                env_config=env_config,
            )

            # Initialize compiler
            if verbose:
                print("[4/7] Compiling Arduino core...")

            compiler = ConfigurableCompiler(context)

            # Determine which libraries are provided locally (symlink://) so we
            # can exclude them from the framework's bundled libraries scan.
            from fbuild.packages.platformio_registry import LibrarySpec

            local_lib_names: set[str] = set()
            raw_lib_deps = env_config.get("lib_deps", "")
            if isinstance(raw_lib_deps, str):
                raw_specs = [d.strip() for d in raw_lib_deps.split("\n") if d.strip()]
            else:
                raw_specs = raw_lib_deps
            for spec_str in raw_specs:
                spec = LibrarySpec.parse(spec_str)
                if spec.is_local:
                    local_lib_names.add(spec.name.lower())

            # Gather framework built-in library include paths (SPI, Wire, etc.)
            # BEFORE compile_core() so all includes are available for compilation.
            framework_include_paths: List[Path] = []
            framework_lib_dirs: List[tuple[str, Path]] = []  # (name, src_dir)
            framework_libs_dir = platform.framework.framework_path / "libraries"
            if not framework_libs_dir.is_dir():
                pio_framework = Path.home() / ".platformio" / "packages" / "framework-arduinoteensy" / "libraries"
                if pio_framework.is_dir():
                    framework_libs_dir = pio_framework
            if framework_libs_dir.is_dir():
                for lib_dir in framework_libs_dir.iterdir():
                    if lib_dir.is_dir():
                        if lib_dir.name.lower() in local_lib_names:
                            if verbose:
                                print(f"      Skipping framework library '{lib_dir.name}' (provided locally)")
                            continue
                        src_sub = lib_dir / "src"
                        if src_sub.is_dir():
                            framework_include_paths.append(src_sub)
                            framework_lib_dirs.append((lib_dir.name, src_sub))
                        else:
                            framework_include_paths.append(lib_dir)
                            framework_lib_dirs.append((lib_dir.name, lib_dir))

            # Initialize the include paths cache, then add framework paths BEFORE
            # core compilation so all includes are available.
            # NOTE: get_include_paths() must be called first to populate the cache,
            # since add_library_includes() is a no-op when cache is None.
            compiler.get_include_paths()
            if framework_include_paths:
                compiler.add_library_includes(framework_include_paths)

            # Compile Arduino core
            core_obj_files = compiler.compile_core()
            core_archive = compiler.create_core_archive(core_obj_files)

            if verbose:
                print(f"      Compiled {len(core_obj_files)} core source files")

            # Handle library dependencies (compiles local libraries)
            library_archives, library_include_paths = self._process_libraries(env_config, build_dir, compiler, platform.toolchain, board_config, verbose, project_dir)

            # Add library include paths to compiler (for sketch compilation)
            if library_include_paths:
                compiler.add_library_includes(library_include_paths)

            # Get src_dir override from platformio.ini (needed for framework lib detection)
            from fbuild.config import PlatformIOConfig

            config_for_src_dir = PlatformIOConfig(project_dir / "platformio.ini")
            src_dir_override = config_for_src_dir.get_src_dir()

            # Compile only the framework libraries that are actually #include'd
            # by the local library or sketch source files.
            if framework_lib_dirs:
                needed_fw_libs = self._detect_needed_framework_libs(
                    framework_lib_dirs,
                    library_include_paths,
                    project_dir,
                    src_dir_override,
                    verbose,
                )
                if needed_fw_libs:
                    if verbose:
                        print(f"[4.7/7] Compiling {len(needed_fw_libs)} framework libraries...")
                    for fw_name, fw_src_dir in needed_fw_libs:
                        try:
                            fw_archive = self._compile_local_library(
                                f"fw_{fw_name}",
                                fw_src_dir,
                                build_dir,
                                compiler,
                                verbose,
                            )
                            if fw_archive is not None:
                                library_archives.append(fw_archive)
                        except KeyboardInterrupt as ke:
                            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

                            handle_keyboard_interrupt_properly(ke)
                            raise
                        except Exception as e:
                            if verbose:
                                print(f"      Skipping framework library '{fw_name}': {e}")
                            continue

            # Find and compile sketch
            sketch_obj_files = self._compile_sketch(project_dir, compiler, start_time, verbose, src_dir_override)
            if sketch_obj_files is None:
                search_dir = project_dir / src_dir_override if src_dir_override else project_dir
                return self._error_result(start_time, f"No .ino sketch file found in {search_dir}")

            # In compiledb-only mode, skip linking — we only need compile commands
            if request.generate_compiledb:
                build_time = time.time() - start_time
                return BuildResultTeensy(
                    success=True,
                    firmware_hex=None,
                    firmware_elf=None,
                    size_info=None,
                    build_time=build_time,
                    message="compile_commands.json generated (compiledb mode)",
                )

            # Initialize linker
            if verbose:
                print("[6/7] Linking firmware...")

            linker = ConfigurableLinker(context)

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
                self._print_success(build_time, firmware_elf, firmware_hex, size_info)

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

            return BuildResultTeensy(success=True, firmware_hex=firmware_hex, firmware_elf=firmware_elf, size_info=size_info, build_time=build_time, message="Build successful (native Teensy build)")

        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            build_time = time.time() - start_time
            import traceback

            error_trace = traceback.format_exc()
            return BuildResultTeensy(success=False, firmware_hex=None, firmware_elf=None, size_info=None, build_time=build_time, message=f"Teensy native build failed: {e}\n\n{error_trace}")

    def _process_libraries(
        self, env_config: dict, build_dir: Path, compiler: ConfigurableCompiler, toolchain: ToolchainTeensy, board_config: BoardConfig, verbose: bool, project_dir: Optional[Path] = None
    ) -> tuple[List[Path], List[Path]]:
        """
        Process and compile library dependencies.

        Handles both local (symlink://) and remote library specs.
        Local libraries are resolved relative to project_dir and their
        include paths are added directly. Remote libraries go through
        the standard LibraryManager download/compile flow.

        Args:
            env_config: Environment configuration
            build_dir: Build directory
            compiler: Configured compiler instance
            toolchain: Teensy toolchain instance
            board_config: Board configuration instance
            verbose: Verbose output mode
            project_dir: Project directory for resolving relative local library paths

        Returns:
            Tuple of (library_archives, library_include_paths)
        """
        lib_deps = env_config.get("lib_deps", "")
        library_archives: List[Path] = []
        library_include_paths: List[Path] = []

        if not lib_deps:
            return library_archives, library_include_paths

        if verbose:
            print("[4.5/7] Processing library dependencies...")

        # Parse lib_deps (can be string or list)
        if isinstance(lib_deps, str):
            lib_spec_strs = [dep.strip() for dep in lib_deps.split("\n") if dep.strip()]
        else:
            lib_spec_strs = lib_deps

        if not lib_spec_strs:
            return library_archives, library_include_paths

        # Separate local and remote library specs
        from fbuild.packages.platformio_registry import LibrarySpec

        remote_specs: List[str] = []

        for spec_str in lib_spec_strs:
            spec = LibrarySpec.parse(spec_str)
            if spec.is_local and spec.local_path is not None:
                # Resolve local library path relative to project_dir
                local_path = spec.local_path
                if not local_path.is_absolute():
                    base_dir = project_dir if project_dir else Path.cwd()
                    local_path = base_dir / local_path
                local_path = local_path.resolve()

                if not local_path.exists():
                    if verbose:
                        print(f"      Warning: Local library path does not exist: {local_path}")
                    continue

                # Determine source directory (prefer src/ subdirectory)
                src_dir = local_path / "src"
                lib_src_dir = src_dir if src_dir.is_dir() else local_path

                # Insert include path at the BEGINNING of the compiler's include list
                # so it takes precedence over framework library headers with the
                # same name (e.g., FastLED's color.h vs ssd1351's color.h).
                # Trampolines use first-occurrence-wins, so order matters.
                library_include_paths.append(lib_src_dir)
                include_cache = compiler.get_include_paths()
                include_cache.insert(0, lib_src_dir)
                if verbose:
                    print(f"      Local library '{spec.name}': {lib_src_dir}")

                # Compile local library source files into a static archive
                archive = self._compile_local_library(
                    spec.name,
                    lib_src_dir,
                    build_dir,
                    compiler,
                    verbose,
                )
                if archive is not None:
                    library_archives.append(archive)
            else:
                remote_specs.append(spec_str)

        # Process remote libraries through the standard LibraryManager
        if remote_specs:
            try:
                library_manager = LibraryManager(build_dir, mode="release")

                # Prepare compilation parameters
                lib_defines = []
                defines_dict = board_config.get_defines()
                for key, value in defines_dict.items():
                    if value:
                        lib_defines.append(f"{key}={value}")
                    else:
                        lib_defines.append(key)

                lib_includes = compiler.get_include_paths()

                compiler_path = toolchain.get_gxx_path()
                if compiler_path is None:
                    raise LibraryError("C++ compiler not found in toolchain")

                if verbose:
                    print(f"      Found {len(remote_specs)} remote library dependencies")
                    print(f"      Compiler path: {compiler_path}")

                libraries = library_manager.ensure_libraries(
                    lib_deps=remote_specs,
                    compiler_path=compiler_path,
                    mcu=board_config.mcu,
                    f_cpu=board_config.f_cpu,
                    defines=lib_defines,
                    include_paths=lib_includes,
                    extra_flags=[],
                    show_progress=verbose,
                )

                library_include_paths.extend(library_manager.get_library_include_paths())
                library_archives.extend(library_manager.get_library_objects())

                if verbose:
                    print(f"      Compiled {len(libraries)} remote libraries")
                    print(f"      Library objects: {len(library_archives)}")

            except LibraryError as e:
                print(f"      Error processing remote libraries: {e}")

        if verbose and library_include_paths:
            print(f"      Total library include paths: {len(library_include_paths)}")

        return library_archives, library_include_paths

    def _detect_needed_framework_libs(
        self,
        framework_lib_dirs: List[tuple[str, Path]],
        library_include_paths: List[Path],
        project_dir: Path,
        src_dir_override: Optional[str],
        verbose: bool,
    ) -> List[tuple[str, Path]]:
        """Scan source files for #include directives that match framework library names.

        Only returns framework libraries whose headers are actually referenced.

        Args:
            framework_lib_dirs: List of (name, src_dir) for all framework libraries
            library_include_paths: Include paths of local libraries to scan
            project_dir: Project directory (for sketch files)
            src_dir_override: Optional src_dir override from platformio.ini
            verbose: Verbose output mode

        Returns:
            Filtered list of (name, src_dir) for needed framework libraries
        """
        import re

        # Build a mapping from header file names to framework library entries
        header_to_lib: dict[str, tuple[str, Path]] = {}
        for fw_name, fw_src_dir in framework_lib_dirs:
            # Scan for header files in the library
            for header in fw_src_dir.glob("*.h"):
                header_to_lib[header.name.lower()] = (fw_name, fw_src_dir)
            for header in fw_src_dir.glob("*.hpp"):
                header_to_lib[header.name.lower()] = (fw_name, fw_src_dir)

        if not header_to_lib:
            return []

        # Collect all source directories to scan
        scan_dirs: List[Path] = list(library_include_paths)
        sketch_dir = project_dir / src_dir_override if src_dir_override else project_dir
        if sketch_dir.is_dir():
            scan_dirs.append(sketch_dir)

        # Scan source files for #include directives
        include_pattern = re.compile(r'#\s*include\s*[<"]([^>"]+)[>"]')
        needed: dict[str, tuple[str, Path]] = {}

        for scan_dir in scan_dirs:
            for ext in ("*.h", "*.hpp", "*.cpp", "*.c", "*.ino", "*.cc"):
                for source_file in scan_dir.rglob(ext):
                    try:
                        content = source_file.read_text(encoding="utf-8", errors="ignore")
                        for match in include_pattern.finditer(content):
                            included = match.group(1).split("/")[-1].lower()
                            if included in header_to_lib:
                                fw_name, fw_src_dir = header_to_lib[included]
                                if fw_name not in needed:
                                    needed[fw_name] = (fw_name, fw_src_dir)
                    except (OSError, UnicodeDecodeError):
                        continue

        if verbose and needed:
            print(f"      Detected {len(needed)} needed framework libraries: {', '.join(needed.keys())}")

        return list(needed.values())

    def _compile_local_library(
        self,
        lib_name: str,
        lib_src_dir: Path,
        build_dir: Path,
        compiler: ConfigurableCompiler,
        verbose: bool,
    ) -> Optional[Path]:
        """Compile a local library's source files into a static archive.

        Finds all .cpp, .c, .S files in the library source directory,
        compiles each to an object file, then archives them into a .a file.

        Args:
            lib_name: Library name (for archive naming and output directory)
            lib_src_dir: Path to the library's source directory
            build_dir: Build directory for output files
            compiler: Configured compiler instance
            verbose: Whether to show progress

        Returns:
            Path to the .a archive, or None if no source files found.
        """
        from fbuild.build.archive_creator import ArchiveCreator

        # Collect all compilable source files, excluding examples/ directories
        excluded_dirs = {"examples", "extras", "test", "tests"}
        source_extensions = {".cpp", ".c", ".S", ".cc", ".cxx"}
        source_files = []
        for ext in source_extensions:
            for f in lib_src_dir.rglob(f"*{ext}"):
                # Skip files in excluded directories
                if not any(part in excluded_dirs for part in f.relative_to(lib_src_dir).parts):
                    source_files.append(f)

        if not source_files:
            if verbose:
                print(f"      No source files found in {lib_src_dir}")
            return None

        if verbose:
            print(f"      Compiling local library '{lib_name}': {len(source_files)} source files")

        # Create output directory for library object files
        sanitized_name = lib_name.lower().replace("/", "_").replace(" ", "_")
        lib_obj_dir = build_dir / "libs" / sanitized_name / "obj"
        lib_obj_dir.mkdir(parents=True, exist_ok=True)

        # Compile each source file
        object_files = []
        for source_file in source_files:
            # Create output path preserving directory structure to avoid name collisions
            relative = source_file.relative_to(lib_src_dir)
            obj_path = lib_obj_dir / relative.with_suffix(".o")
            obj_path.parent.mkdir(parents=True, exist_ok=True)

            # Skip if object file is up-to-date
            if not compiler.needs_rebuild(source_file, obj_path):
                object_files.append(obj_path)
                continue

            compiled_obj = compiler.compile_source(source_file, obj_path)
            object_files.append(compiled_obj)

        # Wait for all async compilations to complete
        compiler.wait_all_jobs()

        if not object_files:
            return None

        # Create static archive
        ar_path = compiler.toolchain.get_ar_path()
        if ar_path is None:
            if verbose:
                print(f"      Warning: ar not found, cannot create archive for '{lib_name}'")
            return None

        archive_path = build_dir / "libs" / sanitized_name / f"lib{sanitized_name}.a"
        archive_creator = ArchiveCreator(show_progress=verbose)
        archive_creator.create_archive(ar_path, archive_path, object_files)

        if verbose:
            print(f"      Archived '{lib_name}': {archive_path.name}")

        return archive_path

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
        if verbose:
            print("[5/7] Compiling sketch...")

        # Determine source directory
        if src_dir_override:
            src_dir = project_dir / src_dir_override
            if verbose:
                print(f"      Using source directory override: {src_dir_override}")
        else:
            # Look for .ino files in the project directory
            src_dir = project_dir

        sketch_files = list(src_dir.glob("*.ino"))
        if not sketch_files and not src_dir_override:
            # Also check src/ directory only if no override was specified
            src_subdir = project_dir / "src"
            if src_subdir.exists():
                sketch_files = list(src_subdir.glob("*.ino"))

        if not sketch_files:
            return None

        sketch_path = sketch_files[0]
        sketch_obj_files = compiler.compile_sketch(sketch_path)

        if verbose:
            print(f"      Compiled {len(sketch_obj_files)} sketch file(s)")

        return sketch_obj_files

    def _print_success(self, build_time: float, firmware_elf: Path, firmware_hex: Path, size_info: Optional[SizeInfo]) -> None:
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
            message_lines.append(f"Program size: {size_info.text + size_info.data} bytes")
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
        return BuildResultTeensy(success=False, firmware_hex=None, firmware_elf=None, size_info=None, build_time=time.time() - start_time, message=message)
