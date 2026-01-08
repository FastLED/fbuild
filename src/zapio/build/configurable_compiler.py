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
from typing import Any, List, Dict, Optional, Union

from ..packages.package import IPackage, IToolchain, IFramework
from .flag_builder import FlagBuilder
from .compilation_executor import CompilationExecutor
from .archive_creator import ArchiveCreator
from .compiler import ICompiler, CompilerError


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
        user_build_flags: Optional[List[str]] = None
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
        """
        self.platform = platform
        self.toolchain = toolchain
        self.framework = framework
        self.board_id = board_id
        self.build_dir = build_dir
        self.show_progress = show_progress
        self.user_build_flags = user_build_flags or []

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
                with open(config_path, 'r') as f:
                    self.config = json.load(f)
            else:
                raise ConfigurableCompilerError(
                    f"No platform configuration found for {self.mcu}. " +
                    f"Expected: {config_path}"
                )
        elif isinstance(platform_config, dict):
            self.config = platform_config
        else:
            # Assume it's a path
            with open(platform_config, 'r') as f:
                self.config = json.load(f)

        # Initialize utility components
        self.flag_builder = FlagBuilder(
            config=self.config,
            board_config=self.board_config,
            board_id=self.board_id,
            variant=self.variant,
            user_build_flags=self.user_build_flags
        )
        self.compilation_executor = CompilationExecutor(
            build_dir=self.build_dir,
            show_progress=self.show_progress
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
            from zapio.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception:
            pass

        # SDK include paths (ESP32-specific)
        if hasattr(self.framework, 'get_sdk_includes'):
            sdk_includes = self.framework.get_sdk_includes(self.mcu)  # type: ignore[attr-defined]
            includes.extend(sdk_includes)

        # Add flash mode specific sdkconfig.h path (ESP32-specific)
        if hasattr(self.framework, 'get_sdk_dir'):
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
            from zapio.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise ConfigurableCompilerError(str(e))

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
            ConfigurableCompilerError: If compilation fails
        """
        # Determine compiler based on file extension
        is_cpp = source_path.suffix in ['.cpp', '.cxx', '.cc']
        compiler_path = self.toolchain.get_gxx_path() if is_cpp else self.toolchain.get_gcc_path()

        if compiler_path is None:
            raise ConfigurableCompilerError(
                f"Compiler path not found for {'C++' if is_cpp else 'C'} compilation"
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

        # Compile using executor
        try:
            return self.compilation_executor.compile_source(
                compiler_path=compiler_path,
                source_path=source_path,
                output_path=output_path,
                compile_flags=compile_flags,
                include_paths=includes
            )
        except KeyboardInterrupt as ke:
            from zapio.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise ConfigurableCompilerError(str(e))

    def compile_sketch(self, sketch_path: Path) -> List[Path]:
        """Compile an Arduino sketch.

        Args:
            sketch_path: Path to .ino file

        Returns:
            List of generated object file paths

        Raises:
            ConfigurableCompilerError: If compilation fails
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
            ConfigurableCompilerError: If compilation fails
        """
        object_files = []

        # Get core sources
        core_sources = self.framework.get_core_sources(self.core)  # type: ignore[attr-defined]

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
            except ConfigurableCompilerError as e:
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
            ConfigurableCompilerError: If archive creation fails
        """
        # Get archiver tool
        ar_path = self.toolchain.get_ar_path()

        if ar_path is None:
            raise ConfigurableCompilerError("Archiver (ar) path not found")

        # Create archive using creator
        try:
            return self.archive_creator.create_core_archive(
                ar_path=ar_path,
                build_dir=self.build_dir,
                object_files=object_files
            )
        except KeyboardInterrupt as ke:
            from zapio.interrupt_utils import handle_keyboard_interrupt_properly
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
            'board_id': self.board_id,
            'mcu': self.mcu,
            'variant': self.variant,
            'build_dir': str(self.build_dir),
            'toolchain_type': self.toolchain.toolchain_type,  # type: ignore[attr-defined]
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

    def compile(
        self,
        source: Path,
        output: Path,
        extra_flags: Optional[List[str]] = None
    ):
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
            return CompileResult(
                success=True,
                object_file=obj_path,
                stdout="",
                stderr="",
                returncode=0
            )
        except ConfigurableCompilerError as e:
            return CompileResult(
                success=False,
                object_file=None,
                stdout="",
                stderr=str(e),
                returncode=1
            )
