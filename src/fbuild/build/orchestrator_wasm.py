"""
WASM-specific build orchestration for Fbuild projects.

This module handles WebAssembly builds using the Emscripten (clang-tool-chain)
toolchain from the FastLED project. It compiles Arduino sketches to
WebAssembly for browser-based execution.

Build pipeline:
  Phase 1: Validate WASM toolchain
  Phase 2: Parse project configuration
  Phase 3: Download and compile library dependencies
  Phase 4: Scan source files
  Phase 5: Compile sources to .o with emcc
  Phase 6: Archive library objects with emar
  Phase 7: Link into .js + .wasm with emcc
  Phase 8: Done

The WASM orchestrator expects `clang-tool-chain-emcc` and `clang-tool-chain-emar`
to be available on PATH, provided by the FastLED WASM toolchain.
"""

import _thread
import json
import logging
import shutil
import time
from pathlib import Path
from typing import TYPE_CHECKING, List, Optional, Tuple

if TYPE_CHECKING:
    from fbuild.build.build_context import BuildParams
    from fbuild.packages.cache import Cache
    from fbuild.packages.library_manager import Library
    from fbuild.platform_configs.board_config_model import BoardConfigModel

from fbuild.build.orchestrator import BuildResult, IBuildOrchestrator
from fbuild.output import log, log_detail, log_phase, set_verbose
from fbuild.subprocess_utils import safe_run

logger = logging.getLogger(__name__)

# Number of build phases
_TOTAL_PHASES = 8


def _find_tool(name: str) -> Optional[Path]:
    """Find a tool on PATH.

    Args:
        name: Tool name to find (e.g., 'clang-tool-chain-emcc')

    Returns:
        Path to the tool or None if not found
    """
    result = shutil.which(name)
    if result is not None:
        return Path(result)
    return None


class OrchestratorWASM(IBuildOrchestrator):
    """Orchestrates WASM builds using Emscripten via clang-tool-chain wrappers.

    This orchestrator compiles Arduino/FastLED sketches to WebAssembly.
    It uses clang-tool-chain-emcc (Emscripten C++ compiler) and
    clang-tool-chain-emar (Emscripten archiver) provided by the
    FastLED WASM toolchain.

    Library dependencies from lib_deps in platformio.ini are downloaded,
    compiled with emcc, and linked into the final WASM binary.
    """

    def __init__(self, cache: Optional["Cache"] = None, verbose: bool = False):
        """Initialize WASM orchestrator.

        Args:
            cache: Cache instance for package management (optional)
            verbose: Enable verbose output
        """
        self.cache = cache
        self.verbose = verbose

    def build(self, request: "BuildParams") -> BuildResult:
        """Execute complete WASM build process.

        Args:
            request: Build request with basic parameters from build_processor

        Returns:
            BuildResult with build status and output paths
        """
        start_time = time.time()
        set_verbose(request.verbose)

        try:
            return self._build_wasm(request, start_time)
        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception as e:
            build_time = time.time() - start_time
            logger.error(f"WASM build failed: {e}")
            return BuildResult(
                success=False,
                hex_path=None,
                elf_path=None,
                size_info=None,
                build_time=build_time,
                message=f"WASM build failed: {e}",
            )

    def _build_wasm(self, request: "BuildParams", start_time: float) -> BuildResult:
        """Internal WASM build implementation.

        Args:
            request: Build request with basic parameters
            start_time: Build start timestamp

        Returns:
            BuildResult with build status and output paths
        """
        project_dir = request.project_dir
        build_dir = request.build_dir
        verbose = request.verbose

        # Phase 1: Validate toolchain
        log_phase(1, _TOTAL_PHASES, "Validating WASM toolchain...")

        emcc = _find_tool("clang-tool-chain-emcc")
        emar = _find_tool("clang-tool-chain-emar")

        if emcc is None:
            return self._error_result(start_time, "clang-tool-chain-emcc not found on PATH. Install the FastLED WASM toolchain.")
        if emar is None:
            return self._error_result(start_time, "clang-tool-chain-emar not found on PATH. Install the FastLED WASM toolchain.")

        log_detail(f"emcc: {emcc}", verbose_only=True)
        log_detail(f"emar: {emar}", verbose_only=True)

        # Phase 2: Parse configuration
        log_phase(2, _TOTAL_PHASES, "Parsing project configuration...")

        from fbuild.config.ini_parser import PlatformIOConfig

        ini_path = project_dir / "platformio.ini"
        if not ini_path.exists():
            return self._error_result(start_time, f"platformio.ini not found in {project_dir}")

        config = PlatformIOConfig(ini_path)
        config.get_env_config(request.env_name)  # Validate environment exists
        build_flags = config.get_build_flags(request.env_name)
        lib_deps = config.get_lib_deps(request.env_name)

        # Load platform config for WASM compiler/linker flags
        from fbuild.platform_configs import load_config as load_platform_config

        platform_config = load_platform_config("wasm")
        if platform_config is None:
            return self._error_result(start_time, "Failed to load WASM platform configuration from wasm.json")

        # Print build profile banner
        from fbuild.build.build_profiles import print_profile_banner

        print_profile_banner(request.profile)

        # Setup build directories
        build_dir.mkdir(parents=True, exist_ok=True)
        src_build_dir = build_dir / "src"
        src_build_dir.mkdir(parents=True, exist_ok=True)

        if request.clean:
            import shutil as shutil_mod

            if build_dir.exists():
                shutil_mod.rmtree(build_dir, ignore_errors=True)
                build_dir.mkdir(parents=True, exist_ok=True)
                src_build_dir.mkdir(parents=True, exist_ok=True)

        # Gather compiler flags from platform config (needed for both libraries and sketch)
        compiler_flags = self._get_compiler_flags(platform_config, request, build_flags)

        # Phase 3: Process library dependencies
        log_phase(3, _TOTAL_PHASES, "Processing library dependencies...")

        library_archives, library_include_paths = self._process_libraries(
            lib_deps=lib_deps,
            build_dir=build_dir,
            emcc=emcc,
            emar=emar,
            compiler_flags=compiler_flags,
            defines=platform_config.defines,
            verbose=verbose,
        )

        if lib_deps:
            log_detail(f"Processed {len(lib_deps)} library dependencies", verbose_only=not verbose)
        else:
            log_detail("No library dependencies", verbose_only=not verbose)

        # Phase 4: Scan source files
        log_phase(4, _TOTAL_PHASES, "Scanning source files...")

        src_dir_override = config.get_src_dir()
        if src_dir_override:
            src_dir = project_dir / src_dir_override
        else:
            src_dir = project_dir / "src"
            if not src_dir.exists():
                src_dir = project_dir

        source_files = self._scan_sources(src_dir)
        if not source_files:
            return self._error_result(start_time, f"No source files found in {src_dir}")

        log_detail(f"Found {len(source_files)} source files", verbose_only=not verbose)

        # Build include paths: project src, project root, then library includes
        include_paths = [str(src_dir)]
        if src_dir != project_dir:
            include_paths.append(str(project_dir))
        for lib_inc in library_include_paths:
            include_paths.append(str(lib_inc))

        # Phase 5: Compile sources
        log_phase(5, _TOTAL_PHASES, "Compiling sources...")

        object_files: List[Path] = []
        for source in source_files:
            obj_path = src_build_dir / f"{source.parent.name}_{source.name}.o"
            success = self._compile_source(
                emcc=emcc,
                source=source,
                obj_path=obj_path,
                compiler_flags=compiler_flags,
                include_paths=include_paths,
                defines=platform_config.defines,
                verbose=verbose,
            )
            if not success:
                return self._error_result(start_time, f"Compilation failed for {source.name}")
            object_files.append(obj_path)

        log_detail(f"Compiled {len(object_files)} objects", verbose_only=not verbose)

        # Phase 6: Archive (optional, for library archives)
        log_phase(6, _TOTAL_PHASES, "Preparing archives...")

        # Collect all object files and archives for linking
        all_link_inputs: List[Path] = list(object_files)
        all_link_inputs.extend(library_archives)

        log_detail(f"Link inputs: {len(object_files)} sketch objects, {len(library_archives)} library archives", verbose_only=not verbose)

        # Phase 7: Link
        log_phase(7, _TOTAL_PHASES, "Linking WASM binary...")

        js_output = build_dir / "firmware.js"
        wasm_output = build_dir / "firmware.wasm"

        linker_flags = self._get_linker_flags(platform_config, request)

        success = self._link(
            emcc=emcc,
            object_files=object_files,
            library_archives=library_archives,
            output_path=js_output,
            linker_flags=linker_flags,
            verbose=verbose,
        )

        if not success:
            return self._error_result(start_time, "Linking failed")

        # Phase 8: Done
        build_time = time.time() - start_time
        log_phase(8, _TOTAL_PHASES, "Build complete!")
        log("")

        # Display output info
        if js_output.exists():
            js_size = js_output.stat().st_size
            log_detail(f"JS output: {js_output} ({js_size} bytes)")
        if wasm_output.exists():
            wasm_size = wasm_output.stat().st_size
            log_detail(f"WASM output: {wasm_output} ({wasm_size} bytes)")

        log(f"Build completed in {build_time:.1f}s")

        return BuildResult(
            success=True,
            hex_path=js_output,  # JS loader is the primary output
            elf_path=wasm_output if wasm_output.exists() else None,
            size_info=None,
            build_time=build_time,
            message="WASM build successful",
        )

    def _process_libraries(
        self,
        lib_deps: List[str],
        build_dir: Path,
        emcc: Path,
        emar: Path,
        compiler_flags: List[str],
        defines: List,
        verbose: bool,
    ) -> Tuple[List[Path], List[Path]]:
        """Download and compile library dependencies.

        Uses LibraryManager to download libraries from GitHub URLs,
        then compiles them with emcc and archives with emar.

        Args:
            lib_deps: Library dependency URLs from platformio.ini
            build_dir: Build directory for storing library artifacts
            emcc: Path to emcc compiler
            emar: Path to emar archiver
            compiler_flags: Compiler flags for library compilation
            defines: Preprocessor defines
            verbose: Enable verbose output

        Returns:
            Tuple of (library_archives, library_include_paths)
        """
        library_archives: List[Path] = []
        library_include_paths: List[Path] = []

        if not lib_deps:
            return library_archives, library_include_paths

        from fbuild.packages.library_manager import LibraryManager

        lib_manager = LibraryManager(build_dir)

        total = len(lib_deps)
        for index, url in enumerate(lib_deps, 1):
            lib_name = lib_manager._extract_library_name(url)
            log_detail(f"[{index}/{total}] {lib_name}")

            # Download library (handles GitHub URLs, caching, extraction)
            library = lib_manager.download_library(url, show_progress=verbose)

            # Collect include paths from this library
            lib_include_dirs = library.get_include_dirs()
            library_include_paths.extend(lib_include_dirs)

            # Check if library needs compilation
            needs_rebuild = self._library_needs_rebuild(library, compiler_flags)

            if needs_rebuild:
                # Compile library sources with emcc
                archive = self._compile_library(
                    library=library,
                    emcc=emcc,
                    emar=emar,
                    compiler_flags=compiler_flags,
                    include_paths=lib_include_dirs,
                    defines=defines,
                    verbose=verbose,
                )
                if archive is not None:
                    library_archives.append(archive)
            else:
                log_detail(f"Library '{library.name}' is up to date (cached)", verbose_only=True)
                if library.archive_file.exists():
                    library_archives.append(library.archive_file)

        return library_archives, library_include_paths

    def _library_needs_rebuild(self, library: "Library", compiler_flags: List[str]) -> bool:
        """Check if a library needs to be rebuilt.

        Args:
            library: Library to check
            compiler_flags: Current compiler flags

        Returns:
            True if the library needs rebuilding
        """
        if not library.archive_file.exists():
            return True

        # Check build info for flag changes
        build_info_file = library.lib_dir / "wasm_build_info.json"
        if not build_info_file.exists():
            return True

        try:
            with open(build_info_file, "r", encoding="utf-8") as f:
                build_info = json.load(f)
            stored_flags = build_info.get("compiler_flags", [])
            if stored_flags != compiler_flags:
                return True
        except (json.JSONDecodeError, OSError):
            return True

        return False

    def _compile_library(
        self,
        library: "Library",
        emcc: Path,
        emar: Path,
        compiler_flags: List[str],
        include_paths: List[Path],
        defines: List,
        verbose: bool,
    ) -> Optional[Path]:
        """Compile a library's source files with emcc and archive with emar.

        Args:
            library: Library to compile
            emcc: Path to emcc compiler
            emar: Path to emar archiver
            compiler_flags: Compiler flags
            include_paths: Include paths for this library
            defines: Preprocessor defines
            verbose: Enable verbose output

        Returns:
            Path to the .a archive, or None if library has no sources
        """
        sources = library.get_source_files()
        if not sources:
            log_detail(f"Library '{library.name}' has no source files (header-only)", verbose_only=True)
            return None

        log_detail(f"Compiling library '{library.name}' ({len(sources)} sources)")

        # Build include flags from library's own include dirs
        include_flags: List[str] = []
        for inc in include_paths:
            include_flags.extend(["-I", str(inc)])

        # Compile each source file
        object_files: List[Path] = []
        for source in sources:
            # Create unique object file name based on relative path
            try:
                rel_path = source.relative_to(library.lib_dir)
                unique_name = str(rel_path).replace("/", "_").replace("\\", "_")
                unique_name = unique_name.rsplit(".", 1)[0]
            except ValueError:
                unique_name = source.stem
            obj_path = library.lib_dir / f"{unique_name}.o"

            # Determine C vs C++ flags
            if source.suffix in [".cpp", ".cc", ".cxx"]:
                std_flag = "-std=gnu++17"
            else:
                std_flag = "-std=gnu17"

            cmd = [str(emcc), "-c", std_flag]
            cmd.extend(compiler_flags)
            cmd.extend(include_flags)

            # Add defines
            for define in defines:
                if isinstance(define, list) and len(define) == 2:
                    cmd.append(f"-D{define[0]}={define[1]}")
                elif isinstance(define, str):
                    if define.startswith("-D"):
                        cmd.append(define)
                    else:
                        cmd.append(f"-D{define}")

            cmd.extend(["-o", str(obj_path), str(source)])

            if verbose:
                log_detail(f"  Compiling: {source.name}")

            result = safe_run(cmd, capture_output=True, text=True)
            if result.returncode != 0:
                logger.error(f"Library compilation failed for {source.name}: {result.stderr}")
                log(f"ERROR: Library compilation failed for {library.name}/{source.name}")
                if result.stderr:
                    log(result.stderr)
                return None

            object_files.append(obj_path)

        # Archive with emar
        archive_path = library.archive_file
        if archive_path.exists():
            archive_path.unlink()

        cmd = [str(emar), "rcs", str(archive_path)] + [str(obj) for obj in object_files]
        result = safe_run(cmd, capture_output=True, text=True)
        if result.returncode != 0:
            logger.error(f"Archive creation failed for {library.name}: {result.stderr}")
            log(f"ERROR: Failed to create archive for {library.name}")
            if result.stderr:
                log(result.stderr)
            return None

        # Save build info for rebuild detection
        build_info_file = library.lib_dir / "wasm_build_info.json"
        build_info = {
            "compiler_flags": compiler_flags,
            "source_count": len(sources),
            "object_files": [str(obj) for obj in object_files],
        }
        with open(build_info_file, "w", encoding="utf-8") as f:
            json.dump(build_info, f, indent=2)

        log_detail(f"Library '{library.name}' compiled ({len(object_files)} objects)")

        return archive_path

    def _scan_sources(self, src_dir: Path) -> List[Path]:
        """Scan for C/C++ source files.

        Args:
            src_dir: Directory to scan for sources

        Returns:
            List of source file paths
        """
        extensions = {".cpp", ".c", ".cc", ".cxx", ".ino"}
        sources: List[Path] = []

        if not src_dir.exists():
            return sources

        for ext in extensions:
            sources.extend(src_dir.rglob(f"*{ext}"))

        return sorted(sources)

    def _get_compiler_flags(self, platform_config: "BoardConfigModel", request: "BuildParams", user_flags: List[str]) -> List[str]:
        """Build compiler flags from platform config and profile.

        Args:
            platform_config: WASM platform configuration
            request: Build request with profile info
            user_flags: User build flags from platformio.ini

        Returns:
            Combined list of compiler flags
        """
        flags: List[str] = []

        # Add common compiler flags from platform config
        flags.extend(platform_config.compiler_flags.common)

        # Add profile-specific flags
        profile_name = request.profile.value
        if profile_name in platform_config.profiles:
            flags.extend(platform_config.profiles[profile_name].compile_flags)

        # Add user build flags from platformio.ini
        flags.extend(user_flags)

        return flags

    def _get_linker_flags(self, platform_config: "BoardConfigModel", request: "BuildParams") -> List[str]:
        """Build linker flags from platform config and profile.

        Args:
            platform_config: WASM platform configuration
            request: Build request with profile info

        Returns:
            Combined list of linker flags
        """
        flags: List[str] = []

        # Add base linker flags from platform config
        flags.extend(platform_config.linker_flags)

        # Add profile-specific link flags
        profile_name = request.profile.value
        if profile_name in platform_config.profiles:
            flags.extend(platform_config.profiles[profile_name].link_flags)

        return flags

    def _compile_source(
        self,
        emcc: Path,
        source: Path,
        obj_path: Path,
        compiler_flags: List[str],
        include_paths: List[str],
        defines: List,
        verbose: bool,
    ) -> bool:
        """Compile a single source file to an object file.

        Args:
            emcc: Path to emcc compiler
            source: Source file to compile
            obj_path: Output object file path
            compiler_flags: Compiler flags
            include_paths: Include search paths
            defines: Preprocessor defines
            verbose: Enable verbose output

        Returns:
            True if compilation succeeded
        """
        cmd = [str(emcc)]

        # Add compiler flags
        cmd.extend(compiler_flags)

        # Add include paths
        for inc in include_paths:
            cmd.extend(["-I", inc])

        # Add defines
        for define in defines:
            if isinstance(define, list) and len(define) == 2:
                cmd.append(f"-D{define[0]}={define[1]}")
            elif isinstance(define, str):
                if define.startswith("-D"):
                    cmd.append(define)
                else:
                    cmd.append(f"-D{define}")

        # Compile to object
        cmd.extend(["-c", str(source), "-o", str(obj_path)])

        if verbose:
            log_detail(f"Compiling: {source.name}")

        result = safe_run(cmd, capture_output=True, text=True)
        if result.returncode != 0:
            logger.error(f"Compilation failed for {source.name}: {result.stderr}")
            log(f"ERROR: Compilation failed for {source.name}")
            if result.stderr:
                log(result.stderr)
            return False

        return True

    def _link(
        self,
        emcc: Path,
        object_files: List[Path],
        library_archives: List[Path],
        output_path: Path,
        linker_flags: List[str],
        verbose: bool,
    ) -> bool:
        """Link object files and library archives into WASM binary.

        Args:
            emcc: Path to emcc compiler (used as linker)
            object_files: Sketch object files to link
            library_archives: Library .a archives to link
            output_path: Output .js file path (WASM generated alongside)
            linker_flags: Linker flags
            verbose: Enable verbose output

        Returns:
            True if linking succeeded
        """
        cmd = [str(emcc)]

        # Add sketch object files
        for obj in object_files:
            cmd.append(str(obj))

        # Add library archives with --whole-archive for symbol visibility
        if library_archives:
            cmd.append("-Wl,--whole-archive")
            for archive in library_archives:
                cmd.append(str(archive))
            cmd.append("-Wl,--no-whole-archive")

        # Add linker flags
        cmd.extend(linker_flags)

        # Output
        cmd.extend(["-o", str(output_path)])

        if verbose:
            log_detail(f"Linking: {output_path.name}")
            log_detail(f"Objects: {len(object_files)} files, Archives: {len(library_archives)}")

        result = safe_run(cmd, capture_output=True, text=True)
        if result.returncode != 0:
            logger.error(f"Linking failed: {result.stderr}")
            log("ERROR: Linking failed")
            if result.stderr:
                log(result.stderr)
            return False

        return True

    def _error_result(self, start_time: float, message: str) -> BuildResult:
        """Create a failed BuildResult.

        Args:
            start_time: Build start timestamp
            message: Error message

        Returns:
            Failed BuildResult
        """
        build_time = time.time() - start_time
        logger.error(message)
        log(f"ERROR: {message}")
        return BuildResult(
            success=False,
            hex_path=None,
            elf_path=None,
            size_info=None,
            build_time=build_time,
            message=message,
        )
