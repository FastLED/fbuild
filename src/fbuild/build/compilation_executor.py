"""Compilation Executor.

This module handles executing compilation commands via subprocess with proper error handling.

Design:
    - Wraps subprocess.run for compilation commands
    - Uses header trampoline cache to avoid Windows command-line length limits
    - Provides clear error messages for compilation failures
    - Supports both C and C++ compilation
    - Integrates sccache for compilation caching
"""

import _thread
import subprocess
import shutil
import platform
import time
from pathlib import Path
from typing import List, Optional, TYPE_CHECKING

from ..packages.header_trampoline_cache import HeaderTrampolineCache
from ..output import log_detail
from ..subprocess_utils import safe_run

if TYPE_CHECKING:
    from ..daemon.compilation_queue import CompilationJobQueue
    from ..packages.cache import Cache


class CompilationError(Exception):
    """Raised when compilation operations fail."""

    pass


class CompilationExecutor:
    """Executes compilation commands with response file support.

    This class handles:
    - Running compiler subprocess commands
    - Generating response files for include paths
    - Handling compilation errors with clear messages
    - Supporting progress display
    """

    def __init__(
        self,
        build_dir: Path,
        show_progress: bool = True,
        use_sccache: bool = True,
        use_trampolines: bool = True,
        cache: Optional["Cache"] = None,
        mcu: Optional[str] = None,
        framework_version: Optional[str] = None,
    ):
        """Initialize compilation executor.

        Args:
            build_dir: Build directory for response files
            show_progress: Whether to show compilation progress
            use_sccache: Whether to use sccache for caching (default: True)
            use_trampolines: Whether to use header trampolines on Windows (default: True)
            cache: Cache object for accessing trampoline directory (optional)
            mcu: MCU variant identifier (e.g., 'esp32c6', 'esp32c3') for MCU-specific caching
            framework_version: Framework version string for cache invalidation
        """
        self.build_dir = build_dir
        self.show_progress = show_progress
        self.mcu = mcu
        self.framework_version = framework_version

        # Disable sccache on Windows due to file locking issues
        # See: https://github.com/anthropics/claude-code/issues/...
        if platform.system() == "Windows":
            if use_sccache and show_progress:
                print("[sccache] Disabled on Windows due to file locking issues")
            self.use_sccache = False
        else:
            self.use_sccache = use_sccache

        self.use_trampolines = use_trampolines
        self.sccache_path: Optional[Path] = None
        self.trampoline_cache: Optional[HeaderTrampolineCache] = None

        # Check if sccache is available
        if self.use_sccache:
            sccache_exe = shutil.which("sccache")
            if sccache_exe:
                self.sccache_path = Path(sccache_exe)
                # Always print sccache status for visibility
                print(f"[sccache] Enabled: {self.sccache_path}")
            else:
                # Try common Windows locations (Git Bash uses /c/ paths)
                common_locations = [
                    Path("/c/tools/python13/Scripts/sccache.exe"),
                    Path("C:/tools/python13/Scripts/sccache.exe"),
                    Path.home() / ".cargo" / "bin" / "sccache.exe",
                ]
                for loc in common_locations:
                    if loc.exists():
                        self.sccache_path = loc
                        print(f"[sccache] Enabled: {self.sccache_path}")
                        break
                else:
                    # Always warn if sccache not found
                    print("[sccache] Warning: not found in PATH, proceeding without cache")

        # Initialize trampoline cache if enabled and on Windows
        if self.use_trampolines and platform.system() == "Windows":
            if cache is not None:
                # Pass cache_root (trampolines_dir), respects dev mode
                cache.ensure_directories()
                self.trampoline_cache = HeaderTrampolineCache(
                    cache_root=cache.trampolines_dir,
                    show_progress=show_progress,
                    mcu_variant=mcu,
                    framework_version=framework_version,
                    platform_name="esp32",
                )
            else:
                # Legacy fallback
                self.trampoline_cache = HeaderTrampolineCache(show_progress=show_progress)

    def compile_source(self, compiler_path: Path, source_path: Path, output_path: Path, compile_flags: List[str], include_paths: List[Path]) -> Path:
        """Compile a single source file.

        Args:
            compiler_path: Path to compiler executable (gcc/g++)
            source_path: Path to source file
            output_path: Path for output object file
            compile_flags: Compilation flags
            include_paths: Include directory paths

        Returns:
            Path to generated object file

        Raises:
            CompilationError: If compilation fails
        """
        if not compiler_path.exists():
            raise CompilationError(f"Compiler not found: {compiler_path}. Ensure toolchain is installed.")

        if not source_path.exists():
            raise CompilationError(f"Source file not found: {source_path}")

        # Ensure output directory exists
        output_path.parent.mkdir(parents=True, exist_ok=True)

        # Apply header trampoline cache on Windows when enabled
        # This resolves Windows CreateProcess 32K limit issues with sccache
        effective_include_paths = include_paths
        if self.trampoline_cache is not None and platform.system() == "Windows":
            # Use trampolines to shorten include paths
            # Exclude ESP-IDF headers that use relative paths that break trampolines
            try:
                exclude_patterns = [
                    "newlib/platform_include",  # Uses #include_next which breaks trampolines
                    "newlib\\platform_include",  # Windows path variant
                    "/bt/",  # ESP32 Bluetooth SDK uses relative includes that break with trampolines
                    "\\bt\\",  # Windows path variant
                    "/hal/esp32",  # Chip-specific hal uses #include_next
                    "\\hal\\esp32",  # Windows path variant
                    "lwip/include/lwip",  # lwip uses #include_next
                    "lwip\\include\\lwip",  # Windows path variant
                    "mbedtls/port/include",  # mbedtls uses #include_next
                    "mbedtls\\port\\include",  # Windows path variant
                ]
                effective_include_paths = self.trampoline_cache.generate_trampolines(include_paths, exclude_patterns=exclude_patterns)
            except KeyboardInterrupt:
                _thread.interrupt_main()
                raise
            except Exception as e:
                if self.show_progress:
                    print(f"[trampolines] Warning: Failed to generate trampolines, using original paths: {e}")
                effective_include_paths = include_paths

        # Convert include paths to flags - ensure no quotes for sccache compatibility
        # GCC response files with quotes cause sccache to treat @file literally
        include_flags = [f"-I{str(inc).replace(chr(92), '/')}" for inc in effective_include_paths]

        # Build compiler command
        cmd = self._build_compile_command(compiler_path, source_path, output_path, compile_flags, include_flags)

        # Execute compilation
        if self.show_progress:
            log_detail(f"Compiling {source_path.name}...")

        try:
            result = safe_run(cmd, capture_output=True, text=True, timeout=60)

            if result.returncode != 0:
                error_msg = f"Compilation failed for {source_path.name}\n"
                error_msg += f"stderr: {result.stderr}\n"
                error_msg += f"stdout: {result.stdout}"
                raise CompilationError(error_msg)

            if self.show_progress and result.stderr:
                log_detail(result.stderr)

            return output_path

        except subprocess.TimeoutExpired as e:
            raise CompilationError(f"Compilation timeout for {source_path.name}") from e
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            if isinstance(e, CompilationError):
                raise
            raise CompilationError(f"Failed to compile {source_path.name}: {e}") from e


    def _build_compile_command(self, compiler_path: Path, source_path: Path, output_path: Path, compile_flags: List[str], include_paths: List[str]) -> List[str]:
        """Build compilation command with optional sccache wrapper.

        Args:
            compiler_path: Path to compiler executable
            source_path: Path to source file
            output_path: Path for output object file
            compile_flags: Compilation flags
            include_paths: Include paths (or include flags if already converted)

        Returns:
            List of command arguments
        """
        # Include paths are already converted to flags (List[str])
        include_flags = include_paths

        # Build compiler command with optional sccache wrapper
        use_sccache = self.sccache_path is not None

        cmd = []
        if use_sccache:
            cmd.append(str(self.sccache_path))
            # Use absolute resolved path for sccache
            # On Windows, sccache needs consistent path format (all backslashes)
            resolved_compiler = compiler_path.resolve()
            compiler_str = str(resolved_compiler)
            # Normalize to Windows backslashes on Windows
            if platform.system() == "Windows":
                compiler_str = compiler_str.replace("/", "\\")
            cmd.append(compiler_str)
        else:
            cmd.append(str(compiler_path))
        cmd.extend(compile_flags)
        cmd.extend(include_flags)  # Trampolines ensure command line stays under 32K limit
        cmd.extend(["-c", str(source_path)])
        cmd.extend(["-o", str(output_path)])

        return cmd


    def preprocess_ino(self, ino_path: Path, output_dir: Path) -> Path:
        """Preprocess .ino file to .cpp file.

        Simple preprocessing: adds Arduino.h include and renames to .cpp.

        Args:
            ino_path: Path to .ino file
            output_dir: Directory for generated .cpp file

        Returns:
            Path to generated .cpp file

        Raises:
            CompilationError: If preprocessing fails
        """
        if not ino_path.exists():
            raise CompilationError(f"Sketch file not found: {ino_path}")

        # Read .ino content
        try:
            with open(ino_path, "r", encoding="utf-8") as f:
                ino_content = f.read()
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise CompilationError(f"Failed to read {ino_path}: {e}") from e

        # Generate .cpp file path
        cpp_path = output_dir / "sketch" / f"{ino_path.stem}.ino.cpp"
        cpp_path.parent.mkdir(parents=True, exist_ok=True)

        # Simple preprocessing: add Arduino.h and content
        cpp_content = "#include <Arduino.h>\n\n" + ino_content

        # Write .cpp file
        try:
            with open(cpp_path, "w", encoding="utf-8") as f:
                f.write(cpp_content)
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise CompilationError(f"Failed to write {cpp_path}: {e}") from e

        if self.show_progress:
            print(f"Preprocessed {ino_path.name} -> {cpp_path.name}")

        return cpp_path

    def compile_source_async(self, compiler_path: Path, source_path: Path, output_path: Path, compile_flags: List[str], include_paths: List[Path], job_queue: "CompilationJobQueue") -> str:
        """Compile a single source file asynchronously via daemon queue.

        This method submits a compilation job to the daemon's CompilationJobQueue
        for parallel execution instead of blocking on subprocess.run().

        Args:
            compiler_path: Path to compiler executable (gcc/g++)
            source_path: Path to source file
            output_path: Path for output object file
            compile_flags: Compilation flags
            include_paths: Include directory paths
            job_queue: CompilationJobQueue from daemon

        Returns:
            Job ID string for tracking the compilation job

        Raises:
            CompilationError: If job submission fails
        """
        from ..daemon.compilation_queue import CompilationJob

        if not compiler_path.exists():
            raise CompilationError(f"Compiler not found: {compiler_path}. Ensure toolchain is installed.")

        if not source_path.exists():
            raise CompilationError(f"Source file not found: {source_path}")

        # Ensure output directory exists
        output_path.parent.mkdir(parents=True, exist_ok=True)

        # Apply header trampoline cache on Windows when enabled
        effective_include_paths = include_paths
        if self.trampoline_cache is not None and platform.system() == "Windows":
            try:
                exclude_patterns = [
                    "newlib/platform_include", "newlib\\platform_include",
                    "/bt/", "\\bt\\",
                    "/hal/esp32", "\\hal\\esp32",
                    "lwip/include/lwip", "lwip\\include\\lwip",
                    "mbedtls/port/include", "mbedtls\\port\\include",
                ]
                effective_include_paths = self.trampoline_cache.generate_trampolines(include_paths, exclude_patterns=exclude_patterns)
            except KeyboardInterrupt:
                _thread.interrupt_main()
                raise
            except Exception as e:
                if self.show_progress:
                    print(f"[trampolines] Warning: Failed to generate trampolines, using original paths: {e}")
                effective_include_paths = include_paths

        # Convert include paths to flags
        include_flags = [f"-I{str(inc).replace(chr(92), '/')}" for inc in effective_include_paths]

        # Build compiler command with optional sccache wrapper
        use_sccache = self.sccache_path is not None

        cmd = []
        if use_sccache:
            cmd.append(str(self.sccache_path))
            resolved_compiler = compiler_path.resolve()
            compiler_str = str(resolved_compiler)
            if platform.system() == "Windows":
                compiler_str = compiler_str.replace("/", "\\")
            cmd.append(compiler_str)
        else:
            cmd.append(str(compiler_path))
        cmd.extend(compile_flags)
        cmd.extend(include_flags)  # Trampolines ensure command line stays under 32K limit
        cmd.extend(["-c", str(source_path)])
        cmd.extend(["-o", str(output_path)])

        # Create and submit compilation job
        job_id = f"compile_{source_path.stem}_{int(time.time() * 1000000)}"

        job = CompilationJob(job_id=job_id, source_path=source_path, output_path=output_path, compiler_cmd=cmd, response_file=None)

        # Submit to queue
        job_queue.submit_job(job)

        return job_id
