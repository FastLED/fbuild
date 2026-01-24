"""
AVR compiler wrapper for building Arduino sketches.

This module provides a wrapper around avr-gcc and avr-g++ for compiling
C and C++ source files to object files with sccache support.
"""

import shutil
from pathlib import Path
from typing import List, Dict, Optional, TYPE_CHECKING
from dataclasses import dataclass

from .compiler import ICompiler, CompilerError
from ..subprocess_utils import safe_run

if TYPE_CHECKING:
    from ..daemon.compilation_queue import CompilationJobQueue


@dataclass
class CompileResult:
    """Result of a compilation operation."""
    success: bool
    object_file: Optional[Path]
    stdout: str
    stderr: str
    returncode: int


class CompilerAVR(ICompiler):
    """
    Wrapper for AVR-GCC compiler.

    Compiles C and C++ source files to object files using avr-gcc and avr-g++
    with appropriate flags for Arduino builds.
    """

    def __init__(
        self,
        avr_gcc: Path,
        avr_gpp: Path,
        mcu: str,
        f_cpu: str,
        includes: List[Path],
        defines: Dict[str, str],
        use_sccache: bool = True,
        compilation_queue: Optional['CompilationJobQueue'] = None
    ):
        """
        Initialize compiler.

        Args:
            avr_gcc: Path to avr-gcc executable
            avr_gpp: Path to avr-g++ executable
            mcu: MCU type (e.g., atmega328p)
            f_cpu: CPU frequency (e.g., 16000000L)
            includes: List of include directories
            defines: Dictionary of preprocessor defines
            use_sccache: Whether to use sccache for caching (default: True)
            compilation_queue: Optional compilation queue for async/parallel compilation
        """
        self.avr_gcc = Path(avr_gcc)
        self.avr_gpp = Path(avr_gpp)
        self.mcu = mcu
        self.f_cpu = f_cpu
        self.includes = [Path(p) for p in includes]
        self.defines = defines
        self.use_sccache = use_sccache
        self.sccache_path: Optional[Path] = None
        self.compilation_queue = compilation_queue
        self.pending_jobs: List[str] = []  # Track async job IDs

        # Check if sccache is available
        if self.use_sccache:
            sccache_exe = shutil.which("sccache")
            if sccache_exe:
                self.sccache_path = Path(sccache_exe)
                print(f"[sccache] Enabled for AVR compiler: {self.sccache_path}")
            else:
                print("[sccache] Warning: not found in PATH, proceeding without cache")

        # Verify tools exist
        if not self.avr_gcc.exists():
            raise CompilerError(f"avr-gcc not found: {self.avr_gcc}")
        if not self.avr_gpp.exists():
            raise CompilerError(f"avr-g++ not found: {self.avr_gpp}")

    def compile_c(
        self,
        source: Path,
        output: Path,
        extra_flags: Optional[List[str]] = None
    ) -> CompileResult:
        """
        Compile C source file.

        Args:
            source: Path to .c source file
            output: Path to output .o object file
            extra_flags: Additional compiler flags

        Returns:
            CompileResult with compilation status
        """
        cmd = self._build_c_command(source, output, extra_flags or [])
        return self._execute_compiler(cmd, output)

    def compile_cpp(
        self,
        source: Path,
        output: Path,
        extra_flags: Optional[List[str]] = None
    ) -> CompileResult:
        """
        Compile C++ source file.

        Args:
            source: Path to .cpp source file
            output: Path to output .o object file
            extra_flags: Additional compiler flags

        Returns:
            CompileResult with compilation status
        """
        cmd = self._build_cpp_command(source, output, extra_flags or [])
        return self._execute_compiler(cmd, output)

    def compile(
        self,
        source: Path,
        output: Path,
        extra_flags: Optional[List[str]] = None
    ) -> CompileResult:
        """
        Compile source file (auto-detects C vs C++).

        Supports dual-mode operation:
        - If compilation_queue is set: submits job asynchronously and returns deferred result
        - If compilation_queue is None: executes synchronously (legacy mode)

        Args:
            source: Path to source file
            output: Path to output .o object file
            extra_flags: Additional compiler flags

        Returns:
            CompileResult with compilation status
            When async mode: returns success=True with deferred=True, actual result via wait_all_jobs()
            When sync mode: returns actual compilation result immediately
        """
        source = Path(source)

        # Async mode: submit to queue and defer result
        if self.compilation_queue is not None:
            # Build the command based on file type
            if source.suffix == '.c':
                cmd = self._build_c_command(source, output, extra_flags or [])
            elif source.suffix in ['.cpp', '.cxx', '.cc']:
                cmd = self._build_cpp_command(source, output, extra_flags or [])
            else:
                raise CompilerError(f"Unknown source file type: {source.suffix}")

            # Submit to async compilation queue
            job_id = self._submit_async_compilation(source, output, cmd)
            self.pending_jobs.append(job_id)

            # Return deferred result (actual result via wait_all_jobs())
            return CompileResult(
                success=True,
                object_file=output,  # Optimistic - will be validated in wait_all_jobs()
                stdout="",
                stderr="",
                returncode=0
            )

        # Sync mode: execute synchronously (legacy behavior)
        if source.suffix == '.c':
            return self.compile_c(source, output, extra_flags)
        elif source.suffix in ['.cpp', '.cxx', '.cc']:
            return self.compile_cpp(source, output, extra_flags)
        else:
            raise CompilerError(f"Unknown source file type: {source.suffix}")

    def compile_sources(
        self,
        sources: List[Path],
        output_dir: Path,
        extra_flags: Optional[List[str]] = None
    ) -> List[Path]:
        """
        Compile multiple source files.

        Args:
            sources: List of source files
            output_dir: Output directory for object files
            extra_flags: Additional compiler flags

        Returns:
            List of compiled object file paths

        Raises:
            CompilerError: If any compilation fails
        """
        output_dir = Path(output_dir)
        output_dir.mkdir(parents=True, exist_ok=True)

        object_files = []

        for source in sources:
            source = Path(source)
            # Generate object file name
            obj_name = source.stem + '.o'
            obj_path = output_dir / obj_name

            # Compile
            result = self.compile(source, obj_path, extra_flags)

            if not result.success:
                raise CompilerError(
                    f"Failed to compile {source}:\n{result.stderr}"
                )

            object_files.append(obj_path)

        return object_files

    def needs_rebuild(self, source: Path, object_file: Path) -> bool:
        """
        Check if source file needs to be recompiled.

        Args:
            source: Source file path
            object_file: Object file path

        Returns:
            True if source is newer than object file
        """
        if not object_file.exists():
            return True

        source_mtime = source.stat().st_mtime
        obj_mtime = object_file.stat().st_mtime

        return source_mtime > obj_mtime

    def _build_c_command(
        self,
        source: Path,
        output: Path,
        extra_flags: List[str]
    ) -> List[str]:
        """Build avr-gcc command for C compilation."""
        cmd = []
        # Prepend sccache if available
        if self.sccache_path:
            cmd.append(str(self.sccache_path))
        cmd.extend([
            str(self.avr_gcc),
            '-c',              # Compile only, don't link
            '-g',              # Include debug symbols
            '-Os',             # Optimize for size
            '-w',              # Suppress warnings (matches Arduino)
            '-std=gnu11',      # C11 with GNU extensions
            '-ffunction-sections',  # Function sections for linker GC
            '-fdata-sections',      # Data sections for linker GC
            '-flto',           # Link-time optimization
            '-fno-fat-lto-objects',  # LTO bytecode only
            f'-mmcu={self.mcu}',    # Target MCU
        ])

        # Add defines
        for key, value in self.defines.items():
            if value:
                cmd.append(f'-D{key}={value}')
            else:
                cmd.append(f'-D{key}')

        # Add F_CPU explicitly
        if 'F_CPU' not in self.defines:
            cmd.append(f'-DF_CPU={self.f_cpu}')

        # Add include paths
        for include in self.includes:
            cmd.append(f'-I{include}')

        # Add extra flags
        cmd.extend(extra_flags)

        # Add source and output
        cmd.extend([str(source), '-o', str(output)])

        return cmd

    def _build_cpp_command(
        self,
        source: Path,
        output: Path,
        extra_flags: List[str]
    ) -> List[str]:
        """Build avr-g++ command for C++ compilation."""
        cmd = []
        # Prepend sccache if available
        if self.sccache_path:
            cmd.append(str(self.sccache_path))
        cmd.extend([
            str(self.avr_gpp),
            '-c',              # Compile only, don't link
            '-g',              # Include debug symbols
            '-Os',             # Optimize for size
            '-w',              # Suppress warnings (matches Arduino)
            '-std=gnu++11',    # C++11 with GNU extensions
            '-fpermissive',    # Allow some non-standard code
            '-fno-exceptions',  # Disable exceptions (no room on AVR)
            '-ffunction-sections',      # Function sections
            '-fdata-sections',          # Data sections
            '-fno-threadsafe-statics',  # No thread safety needed
            '-flto',           # Link-time optimization
            '-fno-fat-lto-objects',  # LTO bytecode only
            f'-mmcu={self.mcu}',        # Target MCU
        ])

        # Add defines
        for key, value in self.defines.items():
            if value:
                cmd.append(f'-D{key}={value}')
            else:
                cmd.append(f'-D{key}')

        # Add F_CPU explicitly
        if 'F_CPU' not in self.defines:
            cmd.append(f'-DF_CPU={self.f_cpu}')

        # Add include paths
        for include in self.includes:
            cmd.append(f'-I{include}')

        # Add extra flags
        cmd.extend(extra_flags)

        # Add source and output
        cmd.extend([str(source), '-o', str(output)])

        return cmd

    def _execute_compiler(
        self,
        cmd: List[str],
        output: Path
    ) -> CompileResult:
        """Execute compiler command."""
        try:
            result = safe_run(
                cmd,
                capture_output=True,
                text=True,
                check=False
            )

            success = result.returncode == 0
            obj_file = output if success and output.exists() else None

            return CompileResult(
                success=success,
                object_file=obj_file,
                stdout=result.stdout,
                stderr=result.stderr,
                returncode=result.returncode
            )

        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            return CompileResult(
                success=False,
                object_file=None,
                stdout='',
                stderr=str(e),
                returncode=-1
            )

    def _submit_async_compilation(
        self,
        source: Path,
        output: Path,
        cmd: List[str]
    ) -> str:
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
            response_file=None  # AVR doesn't use response files for includes
        )

        if self.compilation_queue is None:
            raise CompilerError("Compilation queue not initialized")
        self.compilation_queue.submit_job(job)
        return job_id

    def wait_all_jobs(self) -> List[CompileResult]:
        """
        Wait for all pending async compilation jobs to complete.

        This method must be called after using async compilation mode
        to wait for all submitted jobs and collect their results.

        Returns:
            List of CompileResult for all pending jobs

        Raises:
            CompilerError: If any compilation fails
        """
        if not self.compilation_queue:
            return []

        if not self.pending_jobs:
            return []

        # Wait for all jobs to complete
        self.compilation_queue.wait_for_completion(self.pending_jobs)

        # Collect results
        results = []
        failed_jobs = []

        for job_id in self.pending_jobs:
            job = self.compilation_queue.get_job_status(job_id)

            if job is None:
                # This shouldn't happen
                failed_jobs.append(f"Job {job_id} not found")
                continue

            result = CompileResult(
                success=(job.state.value == "completed"),
                object_file=job.output_path if job.state.value == "completed" else None,
                stdout=job.stdout,
                stderr=job.stderr,
                returncode=job.result_code or -1
            )

            results.append(result)

            if not result.success:
                failed_jobs.append(f"{job.source_path.name}: {job.stderr[:200]}")

        # Clear pending jobs
        self.pending_jobs.clear()

        # Raise error if any jobs failed
        if failed_jobs:
            error_msg = f"Compilation failed for {len(failed_jobs)} file(s):\n"
            error_msg += "\n".join(f"  - {err}" for err in failed_jobs[:5])
            if len(failed_jobs) > 5:
                error_msg += f"\n  ... and {len(failed_jobs) - 5} more"
            raise CompilerError(error_msg)

        return results

    def get_statistics(self) -> Dict[str, int]:
        """
        Get compilation statistics from the queue.

        Returns:
            Dictionary with compilation statistics
        """
        if not self.compilation_queue:
            return {
                "total_jobs": 0,
                "pending": 0,
                "running": 0,
                "completed": 0,
                "failed": 0
            }

        return self.compilation_queue.get_statistics()

    # BaseCompiler interface implementation
    def compile_source(
        self,
        source_path: Path,
        output_path: Optional[Path] = None
    ) -> Path:
        """Compile a single source file to object file.

        Args:
            source_path: Path to .c or .cpp source file
            output_path: Optional path for output .o file

        Returns:
            Path to generated .o file

        Raises:
            CompilerError: If compilation fails
        """
        source_path = Path(source_path)

        # Generate output path if not provided
        if output_path is None:
            output_path = source_path.parent / f"{source_path.stem}.o"

        # Compile the source
        result = self.compile(source_path, output_path)

        if not result.success:
            raise CompilerError(
                f"Failed to compile {source_path}:\n{result.stderr}"
            )

        return output_path

    def get_include_paths(self) -> List[Path]:
        """Get all include paths needed for compilation.

        Returns:
            List of include directory paths
        """
        return self.includes

    def get_compile_flags(self) -> Dict[str, List[str]]:
        """Get compilation flags.

        Returns:
            Dictionary with 'cflags', 'cxxflags', and 'common' keys
        """
        # Common flags for both C and C++
        common = [
            '-c',
            '-g',
            '-Os',
            '-w',
            '-ffunction-sections',
            '-fdata-sections',
            '-flto',
            '-fno-fat-lto-objects',
            f'-mmcu={self.mcu}',
        ]

        # C-specific flags
        cflags = [
            '-std=gnu11',
        ]

        # C++-specific flags
        cxxflags = [
            '-std=gnu++11',
            '-fpermissive',
            '-fno-exceptions',
            '-fno-threadsafe-statics',
        ]

        return {
            'common': common,
            'cflags': cflags,
            'cxxflags': cxxflags,
        }
