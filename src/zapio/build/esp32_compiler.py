"""ESP32 Compiler.

This module handles compilation of ESP32 Arduino sketches and core files.

Compilation Process:
    1. Preprocess .ino files (add function prototypes, Arduino wrapper)
    2. Extract compilation flags from platform.json and board.json
    3. Compile Arduino core sources (.c/.cpp files)
    4. Compile sketch files (.ino preprocessed to .cpp)
    5. Compile library dependencies
    6. Generate .o object files for linking

Compilation Strategy:
    - Use GCC/G++ from toolchain (riscv32-esp-elf-gcc or xtensa-esp32-elf-gcc)
    - Pass correct include paths (core, variant, SDK, libraries)
    - Apply MCU-specific compilation flags
    - Generate position-independent code for ESP32
"""

import re
import subprocess
from pathlib import Path
from typing import List, Dict, Any, Optional

from ..packages.esp32_platform import ESP32Platform
from ..packages.esp32_toolchain import ESP32Toolchain
from ..packages.esp32_framework import ESP32Framework


class ESP32CompilerError(Exception):
    """Raised when ESP32 compilation operations fail."""
    pass


class ESP32Compiler:
    """Manages ESP32 compilation process.

    This class handles:
    - .ino file preprocessing
    - Compilation flag extraction
    - Source file compilation
    - Object file generation
    """

    def __init__(
        self,
        platform: ESP32Platform,
        toolchain: ESP32Toolchain,
        framework: ESP32Framework,
        board_id: str,
        build_dir: Path,
        show_progress: bool = True
    ):
        """Initialize ESP32 compiler.

        Args:
            platform: ESP32 platform instance
            toolchain: ESP32 toolchain instance
            framework: ESP32 framework instance
            board_id: Board identifier (e.g., "esp32-c6-devkitm-1")
            build_dir: Directory for build artifacts
            show_progress: Whether to show compilation progress
        """
        self.platform = platform
        self.toolchain = toolchain
        self.framework = framework
        self.board_id = board_id
        self.build_dir = build_dir
        self.show_progress = show_progress

        # Load board configuration
        self.board_config = platform.get_board_json(board_id)

        # Get MCU type from board config
        self.mcu = self.board_config.get("build", {}).get("mcu", "").lower()

        # Get variant name
        self.variant = self.board_config.get("build", {}).get("variant", "")

        # Cache for compilation flags
        self._compile_flags_cache: Optional[Dict[str, List[str]]] = None
        self._include_paths_cache: Optional[List[Path]] = None

    def preprocess_ino(self, ino_path: Path) -> Path:
        """Preprocess .ino file to .cpp file.

        Arduino .ino files need preprocessing:
        1. Extract function prototypes
        2. Add #include <Arduino.h>
        3. Wrap in standard C++ structure

        Args:
            ino_path: Path to .ino file

        Returns:
            Path to generated .cpp file

        Raises:
            ESP32CompilerError: If preprocessing fails
        """
        if not ino_path.exists():
            raise ESP32CompilerError(f"Sketch file not found: {ino_path}")

        # Read .ino content
        try:
            with open(ino_path, 'r', encoding='utf-8') as f:
                ino_content = f.read()
        except Exception as e:
            raise ESP32CompilerError(f"Failed to read {ino_path}: {e}")

        # Generate .cpp file path
        cpp_path = self.build_dir / "sketch" / f"{ino_path.stem}.ino.cpp"
        cpp_path.parent.mkdir(parents=True, exist_ok=True)

        # Extract function prototypes
        prototypes = self._extract_function_prototypes(ino_content)

        # Generate .cpp content
        cpp_content = self._generate_cpp_from_ino(ino_content, prototypes)

        # Write .cpp file
        try:
            with open(cpp_path, 'w', encoding='utf-8') as f:
                f.write(cpp_content)
        except Exception as e:
            raise ESP32CompilerError(f"Failed to write {cpp_path}: {e}")

        if self.show_progress:
            print(f"Preprocessed {ino_path.name} -> {cpp_path.name}")

        return cpp_path

    def _extract_function_prototypes(self, content: str) -> List[str]:
        """Extract function prototypes from Arduino sketch.

        Args:
            content: Sketch file content

        Returns:
            List of function prototype strings
        """
        prototypes = []

        # Match function definitions (simplified regex)
        # Pattern: return_type function_name(parameters) {
        # This is a simplified version - Arduino IDE does more sophisticated parsing
        function_pattern = re.compile(
            r'^(\w+(?:\s*\*)?)\s+(\w+)\s*\(([^)]*)\)\s*\{',
            re.MULTILINE
        )

        for match in function_pattern.finditer(content):
            return_type = match.group(1).strip()
            func_name = match.group(2).strip()
            params = match.group(3).strip()

            # Skip setup() and loop() as they're standard Arduino functions
            if func_name in ['setup', 'loop']:
                continue

            # Skip if it looks like a class method or already has prototype
            if '::' in func_name or return_type in ['class', 'struct', 'enum']:
                continue

            prototype = f"{return_type} {func_name}({params});"
            prototypes.append(prototype)

        return prototypes

    def _generate_cpp_from_ino(self, ino_content: str, prototypes: List[str]) -> str:
        """Generate .cpp file content from .ino content.

        Args:
            ino_content: Original .ino file content
            prototypes: List of function prototypes

        Returns:
            Complete .cpp file content
        """
        lines = []

        # Add Arduino header
        lines.append('#include <Arduino.h>')
        lines.append('')

        # Add function prototypes
        if prototypes:
            lines.append('// Function prototypes')
            lines.extend(prototypes)
            lines.append('')

        # Add original sketch content
        lines.append('// Original sketch content')
        lines.append(ino_content)

        return '\n'.join(lines)

    def _parse_flag_string(self, flag_string: str) -> List[str]:
        """Parse a flag string that may contain quoted values.

        Args:
            flag_string: String containing compiler flags

        Returns:
            List of individual flags with quotes preserved
        """
        import shlex
        try:
            # Use shlex to properly handle quoted strings
            return shlex.split(flag_string)
        except Exception:
            # Fallback to simple split if shlex fails
            return flag_string.split()

    def get_compile_flags(self) -> Dict[str, List[str]]:
        """Extract compilation flags from board and platform configuration.

        Returns:
            Dictionary with 'cflags', 'cxxflags', and 'common' keys
        """
        if self._compile_flags_cache is not None:
            return self._compile_flags_cache

        flags = {
            'common': [],  # Common flags for both C and C++
            'cflags': [],  # C-specific flags
            'cxxflags': []  # C++-specific flags
        }

        # Get SDK flags directory
        sdk_flags_dir = self.framework.get_sdk_flags_dir(self.mcu)

        # Read defines from SDK flags
        defines_file = sdk_flags_dir / "defines"
        if defines_file.exists():
            with open(defines_file, 'r') as f:
                defines_content = f.read().strip()
                # Parse defines carefully to handle quoted strings
                defines_flags = self._parse_flag_string(defines_content)
                flags['common'].extend(defines_flags)

        # Read C flags from SDK
        c_flags_file = sdk_flags_dir / "c_flags"
        if c_flags_file.exists():
            with open(c_flags_file, 'r') as f:
                c_flags_content = f.read().strip()
                c_flags_parsed = self._parse_flag_string(c_flags_content)
                flags['cflags'].extend(c_flags_parsed)

        # Read C++ flags from SDK
        cpp_flags_file = sdk_flags_dir / "cpp_flags"
        if cpp_flags_file.exists():
            with open(cpp_flags_file, 'r') as f:
                cpp_flags_content = f.read().strip()
                cpp_flags_parsed = self._parse_flag_string(cpp_flags_content)
                flags['cxxflags'].extend(cpp_flags_parsed)

        # Add Arduino-specific defines
        build_config = self.board_config.get("build", {})
        f_cpu = build_config.get("f_cpu", "160000000L")
        board = build_config.get("board", self.board_id.upper().replace("-", "_"))

        flags['common'].extend([
            f'-DF_CPU={f_cpu}',
            '-DARDUINO=10812',  # Arduino version
            '-DESP32',  # ESP32 platform define (required by many libraries like FastLED)
            f'-DARDUINO_{board}',
            '-DARDUINO_ARCH_ESP32',
            f'-DARDUINO_BOARD="{board}"',
            f'-DARDUINO_VARIANT="{self.variant}"',
        ])

        # Add board-specific extra flags if present
        extra_flags = build_config.get("extra_flags", "")
        if extra_flags:
            # Handle both string and list types
            if isinstance(extra_flags, str):
                flag_list = extra_flags.split()
            else:
                flag_list = extra_flags

            for flag in flag_list:
                if flag.startswith('-D'):
                    flags['common'].append(flag)

        self._compile_flags_cache = flags
        return flags

    def get_include_paths(self) -> List[Path]:
        """Get all include paths needed for compilation.

        Returns:
            List of include directory paths
        """
        if self._include_paths_cache is not None:
            return self._include_paths_cache

        includes = []

        # Core include path
        core_dir = self.framework.get_core_dir("esp32")
        includes.append(core_dir)

        # Variant include path
        try:
            variant_dir = self.framework.get_variant_dir(self.variant)
            includes.append(variant_dir)
        except Exception:
            # Variant might not exist
            pass

        # SDK include paths
        sdk_includes = self.framework.get_sdk_includes(self.mcu)
        includes.extend(sdk_includes)

        # Add flash mode specific sdkconfig.h path
        flash_mode = self.board_config.get("build", {}).get("flash_mode", "qio")
        sdk_dir = self.framework.get_sdk_dir()
        flash_config_dir = sdk_dir / self.mcu / f"{flash_mode}_qspi" / "include"
        if flash_config_dir.exists():
            includes.append(flash_config_dir)

        self._include_paths_cache = includes
        return includes

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
            ESP32CompilerError: If compilation fails
        """
        if not source_path.exists():
            raise ESP32CompilerError(f"Source file not found: {source_path}")

        # Determine compiler based on file extension
        is_cpp = source_path.suffix in ['.cpp', '.cxx', '.cc']
        compiler_path = self.toolchain.get_gxx_path() if is_cpp else self.toolchain.get_gcc_path()

        if compiler_path is None or not compiler_path.exists():
            raise ESP32CompilerError(
                f"Compiler not found: {compiler_path}. "
                "Ensure toolchain is installed."
            )

        # Generate output path if not provided
        if output_path is None:
            obj_dir = self.build_dir / "obj"
            obj_dir.mkdir(parents=True, exist_ok=True)
            output_path = obj_dir / f"{source_path.stem}.o"

        # Get compilation flags
        flags = self.get_compile_flags()
        compile_flags = flags['common'].copy()
        if is_cpp:
            compile_flags.extend(flags['cxxflags'])
        else:
            compile_flags.extend(flags['cflags'])

        # Get include paths
        includes = self.get_include_paths()
        # Convert paths to forward slashes for GCC compatibility on Windows
        include_flags = [f"-I{str(inc).replace(chr(92), '/')}" for inc in includes]

        # Write include paths to a response file to avoid command line length limits
        response_file = self.build_dir / "includes.rsp"
        response_file.parent.mkdir(parents=True, exist_ok=True)
        with open(response_file, 'w') as f:
            f.write('\n'.join(include_flags))

        # Build compiler command
        cmd = [str(compiler_path)]
        cmd.extend(compile_flags)
        cmd.append(f"@{response_file}")  # Use response file for includes
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
                error_msg += f"Command: {' '.join(cmd)}\n"
                error_msg += f"stderr: {result.stderr}\n"
                error_msg += f"stdout: {result.stdout}"
                raise ESP32CompilerError(error_msg)

            if self.show_progress and result.stderr:
                # Print warnings
                print(result.stderr)

            return output_path

        except subprocess.TimeoutExpired:
            raise ESP32CompilerError(f"Compilation timeout for {source_path.name}")
        except Exception as e:
            raise ESP32CompilerError(f"Failed to compile {source_path.name}: {e}")

    def compile_sketch(self, sketch_path: Path) -> List[Path]:
        """Compile an Arduino sketch.

        Args:
            sketch_path: Path to .ino file

        Returns:
            List of generated object file paths

        Raises:
            ESP32CompilerError: If compilation fails
        """
        object_files = []

        # Preprocess .ino to .cpp
        cpp_path = self.preprocess_ino(sketch_path)

        # Compile preprocessed .cpp
        obj_path = self.compile_source(cpp_path)
        object_files.append(obj_path)

        return object_files

    def compile_core(self) -> List[Path]:
        """Compile Arduino core sources.

        Returns:
            List of generated object file paths

        Raises:
            ESP32CompilerError: If compilation fails
        """
        object_files = []

        # Get core sources
        core_sources = self.framework.get_core_sources("esp32")

        if self.show_progress:
            print(f"Compiling {len(core_sources)} core source files...")

        # Create core object directory
        core_obj_dir = self.build_dir / "obj" / "core"
        core_obj_dir.mkdir(parents=True, exist_ok=True)

        # Compile each core source
        for source in core_sources:
            try:
                obj_path = core_obj_dir / f"{source.stem}.o"
                compiled_obj = self.compile_source(source, obj_path)
                object_files.append(compiled_obj)
            except ESP32CompilerError as e:
                # Continue on error but report it
                if self.show_progress:
                    print(f"Warning: Failed to compile {source.name}: {e}")

        return object_files

    def create_core_archive(self, object_files: List[Path]) -> Path:
        """Create core.a archive from compiled object files.

        Args:
            object_files: List of object file paths to archive

        Returns:
            Path to generated core.a file

        Raises:
            ESP32CompilerError: If archive creation fails
        """
        if not object_files:
            raise ESP32CompilerError("No object files provided for archive")

        # Get archiver tool
        ar_path = self.toolchain.get_ar_path()
        if ar_path is None or not ar_path.exists():
            raise ESP32CompilerError(
                f"Archiver not found: {ar_path}. "
                "Ensure toolchain is installed."
            )

        # Create archive path
        archive_path = self.build_dir / "core.a"

        # Build archiver command
        # ar rcs core.a obj1.o obj2.o ...
        cmd = [str(ar_path), "rcs", str(archive_path)]
        cmd.extend([str(obj) for obj in object_files])

        # Execute archiver
        if self.show_progress:
            print(f"Creating core.a archive from {len(object_files)} object files...")

        try:
            result = subprocess.run(
                cmd,
                capture_output=True,
                text=True,
                timeout=60
            )

            if result.returncode != 0:
                error_msg = "Archive creation failed\n"
                error_msg += f"Command: {' '.join(cmd)}\n"
                error_msg += f"stderr: {result.stderr}\n"
                error_msg += f"stdout: {result.stdout}"
                raise ESP32CompilerError(error_msg)

            if not archive_path.exists():
                raise ESP32CompilerError(f"Archive was not created: {archive_path}")

            if self.show_progress:
                size = archive_path.stat().st_size
                print(f"✓ Created core.a: {size:,} bytes ({size / 1024 / 1024:.2f} MB)")

            return archive_path

        except subprocess.TimeoutExpired:
            raise ESP32CompilerError("Archive creation timeout")
        except Exception as e:
            raise ESP32CompilerError(f"Failed to create archive: {e}")

    def get_compiler_info(self) -> Dict[str, Any]:
        """Get information about the compiler configuration.

        Returns:
            Dictionary with compiler information
        """
        info = {
            'board_id': self.board_id,
            'mcu': self.mcu,
            'variant': self.variant,
            'build_dir': str(self.build_dir),
            'toolchain_type': self.toolchain.toolchain_type,
            'gcc_path': str(self.toolchain.get_gcc_path()),
            'gxx_path': str(self.toolchain.get_gxx_path()),
        }

        # Add compile flags
        flags = self.get_compile_flags()
        info['compile_flags'] = flags

        # Add include paths
        includes = self.get_include_paths()
        info['include_paths'] = [str(p) for p in includes]
        info['include_count'] = len(includes)

        return info

    def get_base_flags(self) -> List[str]:
        """Get base compiler flags for library compilation.

        Returns:
            List of compiler flags
        """
        flags = self.get_compile_flags()
        base_flags = flags['common'].copy()
        base_flags.extend(flags['cxxflags'])  # Include C++ flags for library compilation
        return base_flags

    def add_library_includes(self, library_includes: List[Path]) -> None:
        """Add library include paths to the compiler.

        Args:
            library_includes: List of library include directory paths
        """
        # Clear cache to force re-computation with new includes
        if self._include_paths_cache is not None:
            self._include_paths_cache.extend(library_includes)
        # If cache not yet built, includes will be added on next get_include_paths call
