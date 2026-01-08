"""Compilation Executor.

This module handles executing compilation commands via subprocess with support
for response files and proper error handling.

Design:
    - Wraps subprocess.run for compilation commands
    - Generates response files for include paths (avoids command line length limits)
    - Provides clear error messages for compilation failures
    - Supports both C and C++ compilation
"""

import subprocess
from pathlib import Path
from typing import List


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

    def __init__(self, build_dir: Path, show_progress: bool = True):
        """Initialize compilation executor.

        Args:
            build_dir: Build directory for response files
            show_progress: Whether to show compilation progress
        """
        self.build_dir = build_dir
        self.show_progress = show_progress

    def compile_source(
        self,
        compiler_path: Path,
        source_path: Path,
        output_path: Path,
        compile_flags: List[str],
        include_paths: List[Path]
    ) -> Path:
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
            raise CompilationError(
                f"Compiler not found: {compiler_path}. Ensure toolchain is installed."
            )

        if not source_path.exists():
            raise CompilationError(f"Source file not found: {source_path}")

        # Ensure output directory exists
        output_path.parent.mkdir(parents=True, exist_ok=True)

        # Convert include paths to flags and write to response file
        include_flags = [f"-I{str(inc).replace(chr(92), '/')}" for inc in include_paths]
        response_file = self._write_response_file(include_flags)

        # Build compiler command
        cmd = [str(compiler_path)]
        cmd.extend(compile_flags)
        cmd.append(f"@{response_file}")
        cmd.extend(['-c', str(source_path)])
        cmd.extend(['-o', str(output_path)])

        # Execute compilation
        if self.show_progress:
            print(f"Compiling {source_path.name}...")

        try:
            result = subprocess.run(
                cmd,
                capture_output=True,
                text=True,
                timeout=60
            )

            if result.returncode != 0:
                error_msg = f"Compilation failed for {source_path.name}\n"
                error_msg += f"stderr: {result.stderr}\n"
                error_msg += f"stdout: {result.stdout}"
                raise CompilationError(error_msg)

            if self.show_progress and result.stderr:
                print(result.stderr)

            return output_path

        except subprocess.TimeoutExpired as e:
            raise CompilationError(f"Compilation timeout for {source_path.name}") from e
        except KeyboardInterrupt as ke:
            from zapio.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            if isinstance(e, CompilationError):
                raise
            raise CompilationError(f"Failed to compile {source_path.name}: {e}") from e

    def _write_response_file(self, include_flags: List[str]) -> Path:
        """Write include paths to response file.

        Response files avoid command line length limits when there are
        many include paths.

        Args:
            include_flags: List of -I include flags

        Returns:
            Path to generated response file
        """
        response_file = self.build_dir / "includes.rsp"
        response_file.parent.mkdir(parents=True, exist_ok=True)

        with open(response_file, 'w') as f:
            f.write('\n'.join(include_flags))

        return response_file

    def preprocess_ino(
        self,
        ino_path: Path,
        output_dir: Path
    ) -> Path:
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
            with open(ino_path, 'r', encoding='utf-8') as f:
                ino_content = f.read()
        except KeyboardInterrupt as ke:
            from zapio.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise CompilationError(f"Failed to read {ino_path}: {e}") from e

        # Generate .cpp file path
        cpp_path = output_dir / "sketch" / f"{ino_path.stem}.ino.cpp"
        cpp_path.parent.mkdir(parents=True, exist_ok=True)

        # Simple preprocessing: add Arduino.h and content
        cpp_content = '#include <Arduino.h>\n\n' + ino_content

        # Write .cpp file
        try:
            with open(cpp_path, 'w', encoding='utf-8') as f:
                f.write(cpp_content)
        except KeyboardInterrupt as ke:
            from zapio.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise CompilationError(f"Failed to write {cpp_path}: {e}") from e

        if self.show_progress:
            print(f"Preprocessed {ino_path.name} -> {cpp_path.name}")

        return cpp_path
