"""Compilation Executor.

This module handles executing compilation commands via subprocess with proper error handling.

Design:
    - Wraps subprocess.run for compilation commands
    - Uses GCC response files (@file) to avoid Windows command-line length limits
    - Provides clear error messages for compilation failures
    - Supports both C and C++ compilation
    - Integrates zccache for compilation caching (handles @file natively)
"""

import os
import platform
import shutil
import subprocess
import time
from pathlib import Path
from typing import TYPE_CHECKING, List, Optional

from fbuild.build.response_file import write_response_file
from fbuild.output import log_detail
from fbuild.subprocess_utils import safe_run

if TYPE_CHECKING:
    from fbuild.build.compile_database import CompileDatabase
    from fbuild.daemon.compilation_queue import CompilationJobQueue


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
        use_zccache: bool = True,
        compile_database: Optional["CompileDatabase"] = None,
        execute_compilations: bool = True,
    ):
        """Initialize compilation executor.

        Args:
            build_dir: Build directory for response files
            show_progress: Whether to show compilation progress
            use_zccache: Whether to use zccache for caching (default: True)
            compile_database: Optional CompileDatabase to capture compilation entries
            execute_compilations: Whether to actually run compilations (False for compiledb-only mode)
        """
        self.build_dir = build_dir
        self.show_progress = show_progress
        self.use_zccache = use_zccache
        self.compile_database = compile_database
        self.execute_compilations = execute_compilations
        self.zccache_path: Optional[str] = None
        self._zccache_session_id: Optional[str] = None

        # Check if zccache is available
        if self.use_zccache:
            zccache_exe = shutil.which("zccache")
            if zccache_exe:
                self.zccache_path = zccache_exe
                print(f"[zccache] Enabled: {self.zccache_path}")
            else:
                print("[zccache] Warning: not found in PATH, proceeding without cache")

    def start_zccache_session(self, compiler_path: Path) -> None:
        """Start a zccache build session for the given compiler.

        Must be called before compilation begins. Sets ZCCACHE_SESSION_ID
        in the process environment so all subprocess calls pick it up.

        Args:
            compiler_path: Path to the compiler executable (gcc/g++)
        """
        if self.zccache_path is None:
            return

        try:
            # Resolve to absolute path — zccache is a native Windows binary
            # and needs Windows-style paths (not MSYS /c/ paths)
            compiler_str = str(compiler_path.resolve())

            result = safe_run(
                [self.zccache_path, "session-start", "--compiler", compiler_str],
                capture_output=True,
                text=True,
                timeout=10,
            )
            session_id = result.stdout.strip()
            if result.returncode == 0 and session_id:
                self._zccache_session_id = session_id
                os.environ["ZCCACHE_SESSION_ID"] = session_id
                print(f"[zccache] Session started: {session_id[:16]}...")
            else:
                print(f"[zccache] Warning: session-start failed: {result.stderr.strip()}")
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            print(f"[zccache] Warning: session-start failed: {e}")

    def end_zccache_session(self) -> None:
        """End the current zccache build session.

        Cleans up ZCCACHE_SESSION_ID from the process environment.
        Should be called after all compilations are complete (typically in a finally block).
        """
        session_id = self._zccache_session_id
        if session_id is None or self.zccache_path is None:
            return

        # Clean up env first regardless of session-end result
        self._zccache_session_id = None
        os.environ.pop("ZCCACHE_SESSION_ID", None)

        try:
            safe_run(
                [self.zccache_path, "session-end", session_id],
                capture_output=True,
                text=True,
                timeout=10,
            )
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception:
            pass  # Best-effort cleanup

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

        # Convert include paths to flags
        include_flags = [f"-I{str(inc).replace(chr(92), '/')}" for inc in include_paths]

        # Write include flags to a response file to avoid Windows 32K command-line limit
        rsp_dir = self.build_dir / "rsp"
        rsp_arg = write_response_file(rsp_dir, include_flags, source_path.stem)

        # Build compiler command with response file instead of inline includes
        cmd = self._build_compile_command(compiler_path, source_path, output_path, compile_flags, [rsp_arg])

        # Record entry in compile database with expanded flags (not @file)
        if self.compile_database is not None:
            from fbuild.build.compile_database import CompileDatabase

            # For compile_commands.json, expand the response file to actual flags
            db_cmd = self._build_compile_command(compiler_path, source_path, output_path, compile_flags, include_flags)
            db_args = CompileDatabase.strip_cache_wrapper(db_cmd)
            self.compile_database.add_entry(
                directory=str(self.build_dir),
                file=str(source_path),
                arguments=db_args,
                output=str(output_path),
            )

        # In compiledb-only mode, skip actual compilation
        if not self.execute_compilations:
            output_path.parent.mkdir(parents=True, exist_ok=True)
            return output_path

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
        """Build compilation command with optional zccache wrapper.

        zccache handles @file arguments natively — no need to bypass for response files.

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

        # zccache handles @file natively, so no response file bypass needed
        use_cache = self.zccache_path is not None

        cmd = []
        if use_cache:
            cmd.append(self.zccache_path)
            # Use absolute resolved path for the compiler
            # On Windows, ensure consistent path format (all backslashes)
            resolved_compiler = compiler_path.resolve()
            compiler_str = str(resolved_compiler)
            if platform.system() == "Windows":
                compiler_str = compiler_str.replace("/", "\\")
            cmd.append(compiler_str)
        else:
            cmd.append(str(compiler_path))
        cmd.extend(compile_flags)
        cmd.extend(include_flags)  # Response file (@file) keeps command line under 32K limit
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
        from fbuild.daemon.compilation_queue import CompilationJob

        if not compiler_path.exists():
            raise CompilationError(f"Compiler not found: {compiler_path}. Ensure toolchain is installed.")

        if not source_path.exists():
            raise CompilationError(f"Source file not found: {source_path}")

        # Ensure output directory exists
        output_path.parent.mkdir(parents=True, exist_ok=True)

        # Convert include paths to flags
        include_flags = [f"-I{str(inc).replace(chr(92), '/')}" for inc in include_paths]

        # Write include flags to a response file to avoid Windows 32K command-line limit
        rsp_dir = self.build_dir / "rsp"
        rsp_arg = write_response_file(rsp_dir, include_flags, f"{source_path.stem}_{int(time.time() * 1000000)}")

        # Build compiler command with response file
        cmd = self._build_compile_command(compiler_path, source_path, output_path, compile_flags, [rsp_arg])

        # Record entry in compile database with expanded flags
        if self.compile_database is not None:
            from fbuild.build.compile_database import CompileDatabase

            db_cmd = self._build_compile_command(compiler_path, source_path, output_path, compile_flags, include_flags)
            db_args = CompileDatabase.strip_cache_wrapper(db_cmd)
            self.compile_database.add_entry(
                directory=str(self.build_dir),
                file=str(source_path),
                arguments=db_args,
                output=str(output_path),
            )

        # In compiledb-only mode, skip actual compilation
        if not self.execute_compilations:
            return f"compiledb_skip_{source_path.stem}_{int(time.time() * 1000000)}"

        # Create and submit compilation job
        job_id = f"compile_{source_path.stem}_{int(time.time() * 1000000)}"

        rsp_path = Path(rsp_arg[1:])  # Strip leading '@'
        job = CompilationJob(job_id=job_id, source_path=source_path, output_path=output_path, compiler_cmd=cmd, response_file=rsp_path)

        # Submit to queue
        job_queue.submit_job(job)

        return job_id
