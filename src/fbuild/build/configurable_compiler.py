"""Configurable Compiler.

This module provides a generic, configuration-driven compiler that can compile
for any platform (ESP32, AVR, etc.) based on platform configuration files.

Design:
    - Loads compilation flags, includes, and settings from JSON/Python config
    - Generic implementation replaces platform-specific compiler classes
    - Same interface as ESP32Compiler for drop-in replacement
"""

import json
from pathlib import Path
from typing import Any, List, Dict, Optional, Union, TYPE_CHECKING

from ..packages.package import IPackage, IToolchain, IFramework
from ..output import ProgressCallback, log_detail
from .flag_builder import FlagBuilder
from .compilation_executor import CompilationExecutor
from .archive_creator import ArchiveCreator
from .compiler import ICompiler, CompilerError

if TYPE_CHECKING:
    from ..daemon.compilation_queue import CompilationJobQueue


class ConfigurableCompilerError(CompilerError):
    """Raised when configurable compilation operations fail."""

    pass


class ConfigurableCompiler(ICompiler):
    """Generic compiler driven by platform configuration.

    This class handles:
    - Loading platform-specific config from JSON
    - Source file compilation with configured flags
    - Object file generation
    - Core archive creation
    """

    def __init__(
        self,
        platform: IPackage,
        toolchain: IToolchain,
        framework: IFramework,
        board_id: str,
        build_dir: Path,
        platform_config: Optional[Union[Dict, Path]] = None,
        show_progress: bool = True,
        user_build_flags: Optional[List[str]] = None,
        compilation_executor: Optional[CompilationExecutor] = None,
        compilation_queue: Optional["CompilationJobQueue"] = None,
        cache: Optional[Any] = None,
    ):
        """Initialize configurable compiler.

        Args:
            platform: Platform instance
            toolchain: Toolchain instance
            framework: Framework instance
            board_id: Board identifier (e.g., "esp32-c6-devkitm-1")
            build_dir: Directory for build artifacts
            platform_config: Platform config dict or path to config JSON file
            show_progress: Whether to show compilation progress
            user_build_flags: Build flags from platformio.ini
            compilation_executor: Optional pre-initialized CompilationExecutor
            compilation_queue: Optional compilation queue for async/parallel compilation
            cache: Optional cache object for header trampoline support
        """
        self.platform = platform
        self.toolchain = toolchain
        self.framework = framework
        self.board_id = board_id
        self.build_dir = build_dir
        self.show_progress = show_progress
        self.user_build_flags = user_build_flags or []
        self.compilation_queue = compilation_queue
        self.cache = cache
        self.pending_jobs: List[str] = []  # Track async job IDs

        # Load board configuration
        self.board_config = platform.get_board_json(board_id)  # type: ignore[attr-defined]

        # Get MCU type from board config
        self.mcu = self.board_config.get("build", {}).get("mcu", "").lower()

        # Get variant name
        self.variant = self.board_config.get("build", {}).get("variant", "")

        # Get core name from board config (defaults to "arduino" if not specified)
        self.core = self.board_config.get("build", {}).get("core", "arduino")

        # Load platform configuration
        if platform_config is None:
            # Try to load from default location
            config_path = Path(__file__).parent.parent / "platform_configs" / f"{self.mcu}.json"
            if config_path.exists():
                with open(config_path, "r") as f:
                    self.config = json.load(f)
            else:
                raise ConfigurableCompilerError(f"No platform configuration found for {self.mcu}. " + f"Expected: {config_path}")
        elif isinstance(platform_config, dict):
            self.config = platform_config
        else:
            # Assume it's a path
            with open(platform_config, "r") as f:
                self.config = json.load(f)

        # Initialize utility components
        self.flag_builder = FlagBuilder(config=self.config, board_config=self.board_config, board_id=self.board_id, variant=self.variant, user_build_flags=self.user_build_flags)
        # Use provided executor or create a new one
        if compilation_executor is not None:
            self.compilation_executor = compilation_executor
        else:
            # Get framework version if available
            framework_version = getattr(self.framework, "version", None)
            self.compilation_executor = CompilationExecutor(
                build_dir=self.build_dir,
                show_progress=self.show_progress,
                cache=self.cache,
                mcu=self.mcu,
                framework_version=framework_version,
            )
        self.archive_creator = ArchiveCreator(show_progress=self.show_progress)

        # Cache for include paths
        self._include_paths_cache: Optional[List[Path]] = None

    def get_compile_flags(self) -> Dict[str, List[str]]:
        """Get compilation flags from configuration.

        Returns:
            Dictionary with 'cflags', 'cxxflags', and 'common' keys
        """
        return self.flag_builder.build_flags()

    def get_include_paths(self) -> List[Path]:
        """Get all include paths needed for compilation.

        Returns:
            List of include directory paths
        """
        if self._include_paths_cache is not None:
            return self._include_paths_cache

        includes = []

        # Core include path
        core_dir = self.framework.get_core_dir(self.core)  # type: ignore[attr-defined]
        includes.append(core_dir)

        # Variant include path
        try:
            variant_dir = self.framework.get_variant_dir(self.variant)  # type: ignore[attr-defined]
            includes.append(variant_dir)
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception:
            pass

        # SDK include paths (ESP32-specific)
        if hasattr(self.framework, "get_sdk_includes"):
            sdk_includes = self.framework.get_sdk_includes(self.mcu)  # type: ignore[attr-defined]
            includes.extend(sdk_includes)

        # STM32-specific system includes (CMSIS, HAL)
        if hasattr(self.framework, "get_stm32_system_includes"):
            # Determine MCU family from MCU name
            mcu_upper = self.mcu.upper()
            if mcu_upper.startswith("STM32F0"):
                mcu_family = "STM32F0xx"
            elif mcu_upper.startswith("STM32F1"):
                mcu_family = "STM32F1xx"
            elif mcu_upper.startswith("STM32F2"):
                mcu_family = "STM32F2xx"
            elif mcu_upper.startswith("STM32F3"):
                mcu_family = "STM32F3xx"
            elif mcu_upper.startswith("STM32F4"):
                mcu_family = "STM32F4xx"
            elif mcu_upper.startswith("STM32F7"):
                mcu_family = "STM32F7xx"
            elif mcu_upper.startswith("STM32G0"):
                mcu_family = "STM32G0xx"
            elif mcu_upper.startswith("STM32G4"):
                mcu_family = "STM32G4xx"
            elif mcu_upper.startswith("STM32H7"):
                mcu_family = "STM32H7xx"
            elif mcu_upper.startswith("STM32L0"):
                mcu_family = "STM32L0xx"
            elif mcu_upper.startswith("STM32L1"):
                mcu_family = "STM32L1xx"
            elif mcu_upper.startswith("STM32L4"):
                mcu_family = "STM32L4xx"
            elif mcu_upper.startswith("STM32L5"):
                mcu_family = "STM32L5xx"
            elif mcu_upper.startswith("STM32U5"):
                mcu_family = "STM32U5xx"
            elif mcu_upper.startswith("STM32WB"):
                mcu_family = "STM32WBxx"
            elif mcu_upper.startswith("STM32WL"):
                mcu_family = "STM32WLxx"
            else:
                mcu_family = "STM32F4xx"  # Default fallback
            system_includes = self.framework.get_stm32_system_includes(mcu_family)  # type: ignore[attr-defined]
            includes.extend(system_includes)

        # Add flash mode specific sdkconfig.h path (ESP32-specific)
        if hasattr(self.framework, "get_sdk_dir"):
            flash_mode = self.board_config.get("build", {}).get("flash_mode", "qio")
            sdk_dir = self.framework.get_sdk_dir()  # type: ignore[attr-defined]

            # Apply SDK fallback for MCUs not fully supported in the platform
            # (e.g., esp32c2 can use esp32c3 SDK)
            from ..packages.sdk_utils import SDKPathResolver

            resolver = SDKPathResolver(sdk_dir, show_progress=False)
            resolved_mcu = resolver._resolve_mcu(self.mcu)

            flash_config_dir = sdk_dir / resolved_mcu / f"{flash_mode}_qspi" / "include"
            if flash_config_dir.exists():
                includes.append(flash_config_dir)

        # Add Arduino built-in libraries (e.g., SPI, Wire, WiFi) for ESP32
        if hasattr(self.framework, "get_libraries_dir"):
            libs_dir = self.framework.get_libraries_dir()
            if libs_dir.exists():
                # Add src subdirectory of each built-in library
                for lib_entry in libs_dir.iterdir():
                    if lib_entry.is_dir() and not lib_entry.name.startswith("."):
                        lib_src = lib_entry / "src"
                        if lib_src.exists():
                            includes.append(lib_src)

        self._include_paths_cache = includes
        return includes

    def preprocess_ino(self, ino_path: Path) -> Path:
        """Preprocess .ino file to .cpp file.

        Args:
            ino_path: Path to .ino file

        Returns:
            Path to generated .cpp file

        Raises:
            ConfigurableCompilerError: If preprocessing fails
        """
        try:
            return self.compilation_executor.preprocess_ino(ino_path, self.build_dir)
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise ConfigurableCompilerError(str(e))

    def compile_source(self, source_path: Path, output_path: Optional[Path] = None) -> Path:
        """Compile a single source file to object file.

        Uses parallel compilation if queue available (jobs != 1),
        otherwise compiles synchronously (jobs=1 explicitly specified).

        Args:
            source_path: Path to .c or .cpp source file
            output_path: Optional path for output .o file

        Returns:
            Path to generated .o file

        Raises:
            ConfigurableCompilerError: If compilation fails
        """
        # Determine compiler based on file extension
        is_cpp = source_path.suffix in [".cpp", ".cxx", ".cc"]
        compiler_path = self.toolchain.get_gxx_path() if is_cpp else self.toolchain.get_gcc_path()

        if compiler_path is None:
            raise ConfigurableCompilerError(f"Compiler path not found for {'C++' if is_cpp else 'C'} compilation")

        # Generate output path if not provided
        if output_path is None:
            obj_dir = self.build_dir / "obj"
            obj_dir.mkdir(parents=True, exist_ok=True)
            output_path = obj_dir / f"{source_path.stem}.o"

        # Get compilation flags
        flags = self.get_compile_flags()
        compile_flags = flags["common"].copy()
        if is_cpp:
            compile_flags.extend(flags["cxxflags"])
        else:
            compile_flags.extend(flags["cflags"])

        # Get include paths
        includes = self.get_include_paths()

        # Parallel mode: submit to queue and return immediately
        if self.compilation_queue is not None:
            import platform
            import _thread
            import logging

            # Apply header trampoline cache on Windows when enabled (same as compilation_executor.py:149-169)
            # This resolves Windows CreateProcess 32K limit issues
            effective_includes = includes
            logging.warning(f"[TRAMPOLINE_DEBUG] compilation_executor={self.compilation_executor}, trampoline_cache={self.compilation_executor.trampoline_cache}, is_windows={platform.system() == 'Windows'}")
            if self.compilation_executor.trampoline_cache is not None and platform.system() == "Windows":
                logging.warning("[TRAMPOLINE_DEBUG] ENTERING trampoline generation block")
                try:
                    exclude_patterns = [
                        "newlib/platform_include",  # Uses #include_next which breaks trampolines
                        "newlib\\platform_include",  # Windows path variant
                        # NOTE: /bt/ exclusion removed - trampolines use absolute paths which work fine
                    ]
                    logging.warning(f"[TRAMPOLINE_DEBUG] Calling generate_trampolines with {len(includes)} includes")
                    effective_includes = self.compilation_executor.trampoline_cache.generate_trampolines(includes, exclude_patterns=exclude_patterns)
                    logging.warning(f"[TRAMPOLINE_DEBUG] After generate_trampolines, got {len(effective_includes)} effective includes")
                except KeyboardInterrupt:
                    _thread.interrupt_main()
                    raise
                except Exception as e:
                    if self.show_progress:
                        print(f"[trampolines] Warning: Failed to generate trampolines, using original paths: {e}")
                    effective_includes = includes

            # Convert include paths to flags
            include_flags = [f"-I{str(inc).replace(chr(92), '/')}" for inc in effective_includes]
            logging.warning(f"[TRAMPOLINE_DEBUG] First include flag: {include_flags[0] if include_flags else 'EMPTY'}")
            # Calculate total command line length
            cmd_preview = " ".join(include_flags)
            logging.warning(f"[TRAMPOLINE_DEBUG] Command line length: {len(cmd_preview)} chars")
            # Build command that would be executed
            cmd = self.compilation_executor._build_compile_command(compiler_path, source_path, output_path, compile_flags, include_flags)

            # Submit to async compilation queue
            job_id = self._submit_async_compilation(source_path, output_path, cmd)
            self.pending_jobs.append(job_id)

            # Return output path optimistically (validated in wait_all_jobs())
            return output_path

        # Serial mode: compile synchronously (only when jobs=1 specified)
        try:
            return self.compilation_executor.compile_source(compiler_path=compiler_path, source_path=source_path, output_path=output_path, compile_flags=compile_flags, include_paths=includes)
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise ConfigurableCompilerError(str(e))

    def compile_sketch(self, sketch_path: Path) -> List[Path]:
        """Compile an Arduino sketch.

        This method handles Arduino sketches that may contain multiple source files:
        - The main .ino file is preprocessed and compiled
        - Additional .cpp files in the sketch directory are also compiled
        - The sketch directory is added to include paths for header file resolution

        Args:
            sketch_path: Path to .ino file

        Returns:
            List of generated object file paths

        Raises:
            ConfigurableCompilerError: If compilation fails
        """
        object_files = []

        # Add sketch directory to include paths so headers like ValidationConfig.h can be found
        sketch_dir = sketch_path.parent
        include_paths = self.get_include_paths()
        if sketch_dir not in include_paths:
            include_paths.insert(0, sketch_dir)  # Add at front for priority

        # Preprocess .ino to .cpp
        cpp_path = self.preprocess_ino(sketch_path)

        # Determine object file path
        obj_dir = self.build_dir / "obj"
        obj_dir.mkdir(parents=True, exist_ok=True)
        obj_path = obj_dir / f"{cpp_path.stem}.o"

        # Skip compilation if object file is up-to-date
        if not self.needs_rebuild(cpp_path, obj_path):
            object_files.append(obj_path)
        else:
            # Compile preprocessed .cpp
            compiled_obj = self.compile_source(cpp_path, obj_path)
            object_files.append(compiled_obj)

        # Find and compile additional .cpp files in the sketch directory
        # (Arduino IDE compiles all .cpp files in the sketch folder)
        for cpp_file in sketch_dir.glob("*.cpp"):
            cpp_obj_path = obj_dir / f"{cpp_file.stem}.o"

            # Skip compilation if object file is up-to-date
            if not self.needs_rebuild(cpp_file, cpp_obj_path):
                object_files.append(cpp_obj_path)
                continue

            try:
                compiled_obj = self.compile_source(cpp_file, cpp_obj_path)
                object_files.append(compiled_obj)
            except ConfigurableCompilerError as e:
                # Re-raise with more context about which file failed
                raise ConfigurableCompilerError(f"Failed to compile sketch source file {cpp_file.name}: {e}")

        # Wait for all async compilations to complete before returning
        # (fixes race condition where linker runs before .o files are written)
        self.wait_all_jobs()

        return object_files

    def compile_core(self, progress_bar: Optional[Any] = None, progress_callback: ProgressCallback | None = None) -> List[Path]:
        """Compile Arduino core sources.

        Args:
            progress_bar: Optional tqdm progress bar to update during compilation
            progress_callback: Optional callback for progress notifications

        Returns:
            List of generated object file paths

        Raises:
            ConfigurableCompilerError: If compilation fails
        """
        object_files = []

        # Get core sources and directory for relative path display
        core_sources = self.framework.get_core_sources(self.core)  # type: ignore[attr-defined]
        core_dir = self.framework.get_core_dir(self.core)  # type: ignore[attr-defined]

        if self.show_progress:
            log_detail(f"Compiling {len(core_sources)} core source files...")

        # Create core object directory
        core_obj_dir = self.build_dir / "obj" / "core"
        core_obj_dir.mkdir(parents=True, exist_ok=True)

        # Disable individual file progress messages when using progress bar
        original_show_progress = self.compilation_executor.show_progress
        if progress_bar is not None:
            self.compilation_executor.show_progress = False

        total_sources = len(core_sources)

        try:
            # Compile each core source
            for idx, source in enumerate(core_sources, 1):
                # Compute relative path for display (especially useful for unity builds)
                try:
                    rel_path_str = str(source.relative_to(core_dir))
                except ValueError:
                    # Fallback to filename if relative path fails
                    rel_path_str = source.name

                # Notify progress callback of file start
                if progress_callback is not None:
                    progress_callback.on_file_start(rel_path_str, idx, total_sources)

                # Update progress bar BEFORE compilation for better UX
                if progress_bar is not None:
                    progress_bar.set_description(f"Compiling {rel_path_str[:30]}")

                try:
                    obj_path = core_obj_dir / f"{source.stem}.o"

                    # Skip compilation if object file is up-to-date
                    if not self.needs_rebuild(source, obj_path):
                        object_files.append(obj_path)
                        if progress_callback is not None:
                            progress_callback.on_file_complete(rel_path_str, idx, total_sources, cached=True)
                        if progress_bar is not None:
                            progress_bar.update(1)
                        continue

                    compiled_obj = self.compile_source(source, obj_path)
                    object_files.append(compiled_obj)
                    if progress_callback is not None:
                        progress_callback.on_file_complete(rel_path_str, idx, total_sources, cached=False)
                    if progress_bar is not None:
                        progress_bar.update(1)
                except ConfigurableCompilerError as e:
                    if self.show_progress:
                        print(f"Warning: Failed to compile {rel_path_str}: {e}")
                    if progress_callback is not None:
                        progress_callback.on_file_complete(rel_path_str, idx, total_sources, cached=False)
                    if progress_bar is not None:
                        progress_bar.update(1)
        finally:
            # Restore original show_progress setting
            self.compilation_executor.show_progress = original_show_progress

        # Wait for all async jobs to complete (if using async mode)
        if hasattr(self, "wait_all_jobs"):
            try:
                self.wait_all_jobs()
            except ConfigurableCompilerError as e:
                raise ConfigurableCompilerError(f"Core compilation failed: {e}")

        return object_files

    def create_core_archive(self, object_files: List[Path]) -> Path:
        """Create core.a archive from compiled object files.

        Args:
            object_files: List of object file paths to archive

        Returns:
            Path to generated core.a file

        Raises:
            ConfigurableCompilerError: If archive creation fails
        """
        # Get archiver tool
        ar_path = self.toolchain.get_ar_path()

        if ar_path is None:
            raise ConfigurableCompilerError("Archiver (ar) path not found")

        # Create archive using creator
        try:
            return self.archive_creator.create_core_archive(ar_path=ar_path, build_dir=self.build_dir, object_files=object_files)
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise ConfigurableCompilerError(str(e))

    def get_compiler_info(self) -> Dict[str, Any]:
        """Get information about the compiler configuration.

        Returns:
            Dictionary with compiler information
        """
        info = {
            "board_id": self.board_id,
            "mcu": self.mcu,
            "variant": self.variant,
            "build_dir": str(self.build_dir),
            "toolchain_type": self.toolchain.toolchain_type,  # type: ignore[attr-defined]
            "gcc_path": str(self.toolchain.get_gcc_path()),
            "gxx_path": str(self.toolchain.get_gxx_path()),
        }

        # Add compile flags
        flags = self.get_compile_flags()
        info["compile_flags"] = flags

        # Add include paths
        includes = self.get_include_paths()
        info["include_paths"] = [str(p) for p in includes]
        info["include_count"] = len(includes)

        return info

    def get_base_flags(self) -> List[str]:
        """Get base compiler flags for library compilation.

        Returns:
            List of compiler flags
        """
        return self.flag_builder.get_base_flags_for_library()

    def add_library_includes(self, library_includes: List[Path]) -> None:
        """Add library include paths to the compiler.

        Args:
            library_includes: List of library include directory paths
        """
        if self._include_paths_cache is not None:
            self._include_paths_cache.extend(library_includes)

    def needs_rebuild(self, source: Path, object_file: Path) -> bool:
        """Check if source file needs to be recompiled.

        Args:
            source: Source file path
            object_file: Object file path

        Returns:
            True if source is newer than object file or object doesn't exist
        """
        if not object_file.exists():
            return True

        source_mtime = source.stat().st_mtime
        object_mtime = object_file.stat().st_mtime

        return source_mtime > object_mtime

    def _submit_async_compilation(self, source: Path, output: Path, cmd: List[str]) -> str:
        """
        Submit compilation job to async queue.

        Args:
            source: Source file path
            output: Output object file path
            cmd: Full compiler command

        Returns:
            Job ID for tracking
        """
        import time
        from ..daemon.compilation_queue import CompilationJob

        job_id = f"compile_{source.stem}_{int(time.time() * 1000000)}"

        job = CompilationJob(
            job_id=job_id,
            source_path=source,
            output_path=output,
            compiler_cmd=cmd,
            response_file=None,  # ConfigurableCompiler doesn't use response files
        )

        if self.compilation_queue is None:
            raise ConfigurableCompilerError("Compilation queue not initialized")
        self.compilation_queue.submit_job(job)
        return job_id

    def wait_all_jobs(self) -> None:
        """
        Wait for all pending async compilation jobs to complete.

        This method must be called after using async compilation mode
        to wait for all submitted jobs and validate their results.

        Raises:
            ConfigurableCompilerError: If any compilation fails
        """
        if not self.compilation_queue:
            return

        if not self.pending_jobs:
            return

        # Wait for all jobs to complete
        self.compilation_queue.wait_for_completion(self.pending_jobs)

        # Collect failed jobs
        failed_jobs = []

        for job_id in self.pending_jobs:
            job = self.compilation_queue.get_job_status(job_id)

            if job is None:
                # This shouldn't happen
                failed_jobs.append(f"Job {job_id} not found")
                continue

            if job.state.value != "completed":
                failed_jobs.append(f"{job.source_path.name}: {job.stderr[:1000]}")

        # Clear pending jobs
        self.pending_jobs.clear()

        # Raise error if any jobs failed
        if failed_jobs:
            error_msg = f"Compilation failed for {len(failed_jobs)} file(s):\n"
            error_msg += "\n".join(f"  - {err}" for err in failed_jobs[:5])
            if len(failed_jobs) > 5:
                error_msg += f"\n  ... and {len(failed_jobs) - 5} more"
            raise ConfigurableCompilerError(error_msg)

    def get_statistics(self) -> Dict[str, int]:
        """
        Get compilation statistics from the queue.

        Returns:
            Dictionary with compilation statistics
        """
        if not self.compilation_queue:
            return {"total_jobs": 0, "pending": 0, "running": 0, "completed": 0, "failed": 0}

        return self.compilation_queue.get_statistics()

    def compile(self, source: Path, output: Path, extra_flags: Optional[List[str]] = None):
        """Compile source file (auto-detects C vs C++).

        Args:
            source: Path to source file
            output: Path to output .o object file
            extra_flags: Additional compiler flags

        Returns:
            CompileResult with compilation status

        Raises:
            ConfigurableCompilerError: If compilation fails
        """
        from .compiler import CompileResult  # Import here to avoid circular dependency

        try:
            obj_path = self.compile_source(source, output)
            return CompileResult(success=True, object_file=obj_path, stdout="", stderr="", returncode=0)
        except ConfigurableCompilerError as e:
            return CompileResult(success=False, object_file=None, stdout="", stderr=str(e), returncode=1)
