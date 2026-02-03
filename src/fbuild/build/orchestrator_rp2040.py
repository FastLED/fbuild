"""
RP2040/RP2350-specific build orchestration for Fbuild projects.

This module handles Raspberry Pi Pico platform builds separately from other platforms,
providing cleaner separation of concerns and better maintainability.
"""

import _thread
import logging
import struct
import time
from pathlib import Path
from typing import TYPE_CHECKING, List, Optional
from dataclasses import dataclass

if TYPE_CHECKING:
    from .build_context import BuildParams

from ..packages import Cache
from ..packages.platform_rp2040 import PlatformRP2040
from ..packages.toolchain_rp2040 import ToolchainRP2040
from ..packages.library_manager import LibraryManager, LibraryError
from ..config.board_config import BoardConfig
from ..cli_utils import BannerFormatter
from ..output import DefaultProgressCallback
from .configurable_compiler import ConfigurableCompiler
from .configurable_linker import ConfigurableLinker
from .linker import SizeInfo
from .orchestrator import IBuildOrchestrator, BuildResult
from .build_utils import safe_rmtree
from .build_state import BuildStateTracker
from .build_info_generator import BuildInfoGenerator

# Module-level logger
logger = logging.getLogger(__name__)


@dataclass
class BuildResultRP2040:
    """Result of an RP2040/RP2350 build operation (internal use)."""

    success: bool
    firmware_uf2: Optional[Path]
    firmware_bin: Optional[Path]
    firmware_elf: Optional[Path]
    size_info: Optional[SizeInfo]
    build_time: float
    message: str


class OrchestratorRP2040(IBuildOrchestrator):
    """
    Orchestrates RP2040/RP2350-specific build process.

    Handles platform initialization, toolchain setup, framework preparation,
    and firmware generation for Raspberry Pi Pico targets.
    """

    # UF2 magic numbers and constants
    UF2_MAGIC_START0 = 0x0A324655  # "UF2\n"
    UF2_MAGIC_START1 = 0x9E5D5157  # Randomly selected
    UF2_MAGIC_END = 0x0AB16F30     # Final magic
    UF2_FLAG_FAMILY_ID_PRESENT = 0x00002000
    RP2040_FAMILY_ID = 0xE48BFF56
    RP2350_FAMILY_ID = 0xE48BFF59  # Different family ID for RP2350

    def __init__(self, cache: Cache, verbose: bool = False):
        """
        Initialize RP2040/RP2350 orchestrator.

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
        from ..config import PlatformIOConfig

        # Extract from request
        project_dir = request.project_dir
        env_name = request.env_name

        # Parse platformio.ini to get environment configuration
        ini_path = project_dir / "platformio.ini"
        if not ini_path.exists():
            return BuildResult(
                success=False,
                hex_path=None,
                elf_path=None,
                size_info=None,
                build_time=0.0,
                message=f"platformio.ini not found in {project_dir}"
            )

        try:
            config = PlatformIOConfig(ini_path)

            env_config = config.get_env_config(env_name)
            board_id = env_config.get("board", "rpipico")
            build_flags = config.get_build_flags(env_name)
            lib_deps = config.get_lib_deps(env_name)

            # Call internal build method
            rp2040_result = self._build_rp2040(
                board_id, env_config, build_flags, lib_deps, request
            )

            # Convert BuildResultRP2040 to BuildResult
            # Note: hex_path maps to uf2_path for RP2040/RP2350
            return BuildResult(
                success=rp2040_result.success,
                hex_path=rp2040_result.firmware_uf2,  # UF2 is the primary firmware format
                elf_path=rp2040_result.firmware_elf,
                size_info=rp2040_result.size_info,
                build_time=rp2040_result.build_time,
                message=rp2040_result.message
            )

        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception as e:
            return BuildResult(
                success=False,
                hex_path=None,
                elf_path=None,
                size_info=None,
                build_time=0.0,
                message=f"Failed to parse configuration: {e}"
            )

    def _build_rp2040(
        self,
        board_id: str,
        env_config: dict,
        build_flags: List[str],
        lib_deps: List[str],
        request: "BuildParams",
    ) -> BuildResultRP2040:
        """
        Execute complete RP2040/RP2350 build process (internal implementation).

        Args:
            board_id: Board ID (e.g., rpipico, rpipico2)
            env_config: Environment configuration dict
            build_flags: User build flags from platformio.ini
            lib_deps: Library dependencies from platformio.ini
            request: Build request with basic parameters

        Returns:
            BuildResultRP2040 with build status and output paths
        """
        start_time = time.time()

        # Extract from request
        project_dir = request.project_dir
        env_name = request.env_name
        verbose = request.verbose
        build_dir = request.build_dir

        try:
            # Get board configuration
            from ..config.board_config import BoardConfig

            if verbose:
                logger.info("[2/7] Loading board configuration...")
            else:
                logger.info("Loading board configuration...")

            board_config = BoardConfig.from_board_id(board_id)

            # Print build profile banner
            from .build_profiles import print_profile_banner
            print_profile_banner(request.profile)

            # Initialize platform
            if verbose:
                logger.info("[3/7] Initializing RP2040/RP2350 platform...")
            else:
                logger.info("Initializing RP2040/RP2350 platform...")

            platform = PlatformRP2040(
                self.cache,
                board_config.mcu,
                show_progress=True
            )
            platform.ensure_package()

            if verbose:
                logger.info(f"      Board: {board_id}")
                logger.info(f"      MCU: {board_config.mcu}")
                logger.info(f"      CPU Frequency: {board_config.f_cpu}")

            # Ensure build directory exists
            build_dir.mkdir(parents=True, exist_ok=True)

            # Check build state and invalidate cache if needed
            if verbose:
                logger.info("[3.5/7] Checking build configuration state...")

            state_tracker = BuildStateTracker(build_dir)
            needs_rebuild, reasons, current_state = state_tracker.check_invalidation(
                platformio_ini_path=project_dir / "platformio.ini",
                platform="raspberrypi",
                board=board_id,
                framework=env_config.get('framework', 'arduino'),
                toolchain_version=platform.toolchain.version,
                framework_version=platform.framework.version,
                platform_version=f"rp2040-{platform.framework.version}",
                build_flags=build_flags,
                lib_deps=lib_deps,
            )

            if needs_rebuild:
                if verbose:
                    logger.info("      Build cache invalidated:")
                    for reason in reasons:
                        logger.info(f"        - {reason}")
                    logger.info("      Cleaning build artifacts...")
                # Clean build artifacts to force rebuild
                if build_dir.exists():
                    safe_rmtree(build_dir)
                # Recreate build directory
                build_dir.mkdir(parents=True, exist_ok=True)
            else:
                if verbose:
                    logger.info("      Build configuration unchanged, using cached artifacts")

            # Initialize compilation executor
            from .compilation_executor import CompilationExecutor
            compilation_executor = CompilationExecutor(
                build_dir=build_dir,
                show_progress=verbose,
                cache=self.cache,
                mcu=board_config.mcu,
                framework_version=platform.framework.version,
            )

            # Load board JSON and platform config ONCE (not redundantly in compiler/linker)
            board_json = platform.get_board_json(board_id)
            from .. import platform_configs
            platform_config = platform_configs.load_config(board_config.mcu)
            if platform_config is None:
                return self._error_result(
                    start_time,
                    f"No platform configuration found for {board_config.mcu}. Available: {platform_configs.list_available_configs()}"
                )

            # Extract variant and core from board config
            variant = board_json.get("build", {}).get("variant", "")
            core = board_json.get("build", {}).get("core", "rp2040")

            # Create full BuildContext with all configuration loaded once
            from .build_context import BuildContext
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
                env_config=env_config,
            )

            # Initialize compiler
            if verbose:
                logger.info("[4/7] Compiling Arduino core...")
            else:
                logger.info("Compiling Arduino core...")

            compiler = ConfigurableCompiler(context)

            # Create progress callback for detailed file-by-file tracking
            progress_callback = DefaultProgressCallback(verbose_only=not verbose)

            # Compile Arduino core with progress bar
            if verbose:
                core_obj_files = compiler.compile_core(progress_callback=progress_callback)
            else:
                # Use tqdm progress bar for non-verbose mode
                from tqdm import tqdm

                # Get number of core source files for progress tracking
                core_sources = platform.framework.get_core_sources("rp2040")
                total_files = len(core_sources)

                # Create progress bar
                with tqdm(
                    total=total_files,
                    desc='Compiling Arduino core',
                    unit='file',
                    ncols=80,
                    leave=False
                ) as pbar:
                    core_obj_files = compiler.compile_core(progress_bar=pbar, progress_callback=progress_callback)

                # Print completion message
                logger.info(f"Compiled {len(core_obj_files)} core files")

            core_archive = compiler.create_core_archive(core_obj_files)

            if verbose:
                logger.info(f"      Compiled {len(core_obj_files)} core source files")

            # Handle library dependencies (if any)
            library_archives, library_include_paths = self._process_libraries(
                env_config, build_dir, compiler, platform.toolchain, board_config, verbose, project_dir=project_dir
            )

            # Add library include paths to compiler
            if library_include_paths:
                compiler.add_library_includes(library_include_paths)

            # Get src_dir override from platformio.ini
            from ..config import PlatformIOConfig
            config_for_src_dir = PlatformIOConfig(project_dir / "platformio.ini")
            src_dir_override = config_for_src_dir.get_src_dir()

            # Find and compile sketch
            sketch_obj_files = self._compile_sketch(project_dir, compiler, start_time, verbose, src_dir_override)
            if sketch_obj_files is None:
                search_dir = project_dir / src_dir_override if src_dir_override else project_dir
                return self._error_result(
                    start_time,
                    f"No .ino sketch file found in {search_dir}"
                )

            # Initialize linker
            if verbose:
                logger.info("[6/7] Linking firmware...")
            else:
                logger.info("Linking firmware...")

            linker = ConfigurableLinker(context)

            # Link firmware
            firmware_elf = linker.link(sketch_obj_files, core_archive, library_archives=library_archives)

            # Generate bin file (intermediate)
            if verbose:
                logger.info("[7/7] Generating firmware...")
            else:
                logger.info("Generating firmware...")

            firmware_bin = linker.generate_bin(firmware_elf)

            # Generate UF2 file (final format for RP2040/RP2350)
            firmware_uf2 = self._generate_uf2(firmware_bin, board_config.mcu, verbose)

            # Get size info
            size_info = linker.get_size_info(firmware_elf)

            build_time = time.time() - start_time

            if verbose:
                self._print_success(
                    build_time, firmware_elf, firmware_uf2, size_info
                )

            # Save build state for future cache validation
            if verbose:
                logger.info("[7.5/7] Saving build state...")
            state_tracker.save_state(current_state)

            # Generate build_info.json
            build_info_generator = BuildInfoGenerator(build_dir)
            # Parse f_cpu from string (e.g., "133000000L") to int
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
                platform="raspberrypi",
                f_cpu=f_cpu_int,
                build_time=build_time,
                elf_path=firmware_elf,
                bin_path=firmware_bin,
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
                logger.info(f"      Build info saved to {build_info_generator.build_info_path}")

            return BuildResultRP2040(
                success=True,
                firmware_uf2=firmware_uf2,
                firmware_bin=firmware_bin,
                firmware_elf=firmware_elf,
                size_info=size_info,
                build_time=build_time,
                message="Build successful (native RP2040/RP2350 build)"
            )

        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            build_time = time.time() - start_time
            import traceback
            error_trace = traceback.format_exc()
            return BuildResultRP2040(
                success=False,
                firmware_uf2=None,
                firmware_bin=None,
                firmware_elf=None,
                size_info=None,
                build_time=build_time,
                message=f"RP2040/RP2350 native build failed: {e}\n\n{error_trace}"
            )

    def _generate_uf2(self, bin_path: Path, mcu: str, verbose: bool = False) -> Path:
        """Generate UF2 file from BIN file.

        UF2 (USB Flashing Format) is used by RP2040/RP2350 bootloaders.

        Args:
            bin_path: Path to input BIN file
            mcu: MCU type ("rp2040" or "rp2350")
            verbose: Verbose output mode

        Returns:
            Path to generated UF2 file

        Raises:
            Exception: If UF2 generation fails
        """
        uf2_path = bin_path.parent / f"{bin_path.stem}.uf2"

        if verbose:
            logger.info(f"      Generating UF2 file: {uf2_path.name}")

        # Select family ID based on MCU
        family_id = self.RP2350_FAMILY_ID if mcu.lower() == "rp2350" else self.RP2040_FAMILY_ID

        # Read binary data
        with open(bin_path, 'rb') as f:
            bin_data = f.read()

        # UF2 block size is 256 bytes of data per block
        block_size = 256
        num_blocks = (len(bin_data) + block_size - 1) // block_size

        # RP2040/RP2350 flash starts at 0x10000000
        base_address = 0x10000000

        with open(uf2_path, 'wb') as f:
            for block_num in range(num_blocks):
                # Get data for this block (pad if needed)
                offset = block_num * block_size
                block_data = bin_data[offset:offset + block_size]
                if len(block_data) < block_size:
                    block_data += b'\x00' * (block_size - len(block_data))

                # Calculate target address
                target_addr = base_address + offset

                # Build UF2 block (512 bytes total)
                uf2_block = struct.pack('<I', self.UF2_MAGIC_START0)      # Magic start 0
                uf2_block += struct.pack('<I', self.UF2_MAGIC_START1)     # Magic start 1
                uf2_block += struct.pack('<I', self.UF2_FLAG_FAMILY_ID_PRESENT)  # Flags
                uf2_block += struct.pack('<I', target_addr)               # Target address
                uf2_block += struct.pack('<I', block_size)                # Payload size
                uf2_block += struct.pack('<I', block_num)                 # Block number
                uf2_block += struct.pack('<I', num_blocks)                # Total blocks
                uf2_block += struct.pack('<I', family_id)                 # Family ID
                uf2_block += block_data                                   # Data (256 bytes)
                uf2_block += struct.pack('<I', self.UF2_MAGIC_END)        # Magic end

                # Pad to 512 bytes (476 bytes already written, need 36 more)
                uf2_block += b'\x00' * (512 - len(uf2_block))

                f.write(uf2_block)

        if verbose:
            logger.info(f"      UF2 file generated: {num_blocks} blocks, {len(bin_data)} bytes")

        return uf2_path

    def _process_libraries(
        self,
        env_config: dict,
        build_dir: Path,
        compiler: ConfigurableCompiler,
        toolchain: ToolchainRP2040,
        board_config: BoardConfig,
        verbose: bool,
        project_dir: Optional[Path] = None
    ) -> tuple[List[Path], List[Path]]:
        """
        Process and compile library dependencies.

        Args:
            env_config: Environment configuration
            build_dir: Build directory
            compiler: Configured compiler instance
            toolchain: RP2040 toolchain instance
            board_config: Board configuration instance
            verbose: Verbose output mode
            project_dir: Optional project directory for resolving relative library paths

        Returns:
            Tuple of (library_archives, library_include_paths)
        """
        lib_deps = env_config.get('lib_deps', '')
        library_archives = []
        library_include_paths = []

        if not lib_deps:
            return library_archives, library_include_paths

        if verbose:
            logger.info("[4.5/7] Processing library dependencies...")

        # Parse lib_deps (can be string or list)
        if isinstance(lib_deps, str):
            lib_specs = [dep.strip() for dep in lib_deps.split('\n') if dep.strip()]
        else:
            lib_specs = lib_deps

        if not lib_specs:
            return library_archives, library_include_paths

        try:
            # Initialize library manager
            library_manager = LibraryManager(build_dir, mode="release")

            # Prepare compilation parameters
            lib_defines = []
            defines_dict = board_config.get_defines()
            for key, value in defines_dict.items():
                if value:
                    lib_defines.append(f"{key}={value}")
                else:
                    lib_defines.append(key)

            # Get include paths from compiler configuration
            lib_includes = compiler.get_include_paths()

            # Get compiler path from toolchain (use C++ compiler for libraries)
            compiler_path = toolchain.get_gxx_path()
            if compiler_path is None:
                raise LibraryError("C++ compiler not found in toolchain")

            if verbose:
                logger.info(f"      Found {len(lib_specs)} library dependencies")

            # Ensure all libraries are downloaded and compiled
            libraries = library_manager.ensure_libraries(
                lib_deps=lib_specs,
                compiler_path=compiler_path,
                mcu=board_config.mcu,
                f_cpu=board_config.f_cpu,
                defines=lib_defines,
                include_paths=lib_includes,
                extra_flags=[],
                show_progress=verbose
            )

            # Get library artifacts
            library_include_paths = library_manager.get_library_include_paths()
            library_archives = library_manager.get_library_objects()

            if verbose:
                logger.info(f"      Compiled {len(libraries)} libraries")

        except LibraryError as e:
            logger.warning(f"      Error processing libraries: {e}")
            # Continue build without libraries
            library_archives = []
            library_include_paths = []

        return library_archives, library_include_paths

    def _compile_sketch(
        self,
        project_dir: Path,
        compiler: ConfigurableCompiler,
        start_time: float,
        verbose: bool,
        src_dir_override: Optional[str] = None
    ) -> Optional[List[Path]]:
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
            logger.info("[5/7] Compiling sketch...")

        # Determine source directory
        if src_dir_override:
            src_dir = project_dir / src_dir_override
            if verbose:
                logger.info(f"      Using source directory override: {src_dir_override}")
        else:
            src_dir = project_dir

        # Look for .ino files in the source directory
        sketch_files = list(src_dir.glob("*.ino"))
        if not sketch_files:
            # Also check src/ directory
            alt_src_dir = project_dir / "src"
            if alt_src_dir.exists() and not src_dir_override:
                sketch_files = list(alt_src_dir.glob("*.ino"))

        if not sketch_files:
            return None

        sketch_path = sketch_files[0]
        sketch_obj_files = compiler.compile_sketch(sketch_path)

        if verbose:
            logger.info(f"      Compiled {len(sketch_obj_files)} sketch file(s)")

        return sketch_obj_files

    def _error_result(self, start_time: float, message: str) -> BuildResultRP2040:
        """Create error result."""
        return BuildResultRP2040(
            success=False,
            firmware_uf2=None,
            firmware_bin=None,
            firmware_elf=None,
            size_info=None,
            build_time=time.time() - start_time,
            message=message
        )

    def _print_success(
        self,
        build_time: float,
        firmware_elf: Path,
        firmware_uf2: Path,
        size_info: Optional[SizeInfo]
    ) -> None:
        """
        Print build success message.

        Args:
            build_time: Total build time
            firmware_elf: Path to firmware ELF
            firmware_uf2: Path to firmware UF2
            size_info: Size information
        """
        # Build success message
        message_lines = ["BUILD SUCCESSFUL!"]
        message_lines.append(f"Build time: {build_time:.2f}s")
        message_lines.append(f"Firmware ELF: {firmware_elf}")
        message_lines.append(f"Firmware UF2: {firmware_uf2}")

        BannerFormatter.print_banner("\n".join(message_lines), width=60, center=False)

        # Print size information if available
        if size_info:
            print()
            from .build_utils import SizeInfoPrinter
            SizeInfoPrinter.print_size_info(size_info)
            print()
