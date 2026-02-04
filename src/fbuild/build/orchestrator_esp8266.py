"""
ESP8266-specific build orchestration for Fbuild projects.

This module handles ESP8266 platform builds, providing simplified firmware
generation compared to ESP32 (no bootloader or partition table needed).
"""

import _thread
import logging
import time
from pathlib import Path
from typing import TYPE_CHECKING, List, Optional
from dataclasses import dataclass

if TYPE_CHECKING:
    from .build_context import BuildParams

from ..packages import Cache
from ..packages.platform_esp8266 import PlatformESP8266
from ..packages.toolchain_esp8266 import ToolchainESP8266
from ..packages.framework_esp8266 import FrameworkESP8266
from ..cli_utils import BannerFormatter
from .configurable_compiler import ConfigurableCompiler
from .configurable_linker import ConfigurableLinker
from .linker import SizeInfo
from .orchestrator import IBuildOrchestrator, BuildResult
from .build_state import BuildStateTracker
from .build_info_generator import BuildInfoGenerator
from ..output import log_phase, log_detail, log_warning, DefaultProgressCallback

# Module-level logger
logger = logging.getLogger(__name__)


@dataclass
class BuildResultESP8266:
    """Result of an ESP8266 build operation (internal use)."""

    success: bool
    firmware_bin: Optional[Path]
    firmware_elf: Optional[Path]
    size_info: Optional[SizeInfo]
    build_time: float
    message: str


class OrchestratorESP8266(IBuildOrchestrator):
    """
    Orchestrates ESP8266-specific build process.

    Handles platform initialization, toolchain setup, framework preparation,
    and firmware generation for ESP8266 targets.

    Key differences from ESP32:
    - Uses xtensa-lx106-elf-gcc toolchain (not riscv32-esp or xtensa-esp32)
    - No partition table (ESP8266 has simpler memory layout)
    - No bootloader generation (ESP8266 uses different boot process)
    - Flash offset is 0x0 for firmware (not 0x10000 like ESP32)
    """

    def __init__(self, cache: Cache, verbose: bool = False):
        """
        Initialize ESP8266 orchestrator.

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
            board_id = env_config.get("board", "")
            build_flags = config.get_build_flags(env_name)

            # Add debug logging for lib_deps
            logger.debug(f"[ORCHESTRATOR] About to call config.get_lib_deps('{env_name}')")
            lib_deps = config.get_lib_deps(env_name)
            logger.debug(f"[ORCHESTRATOR] get_lib_deps returned: {lib_deps}")

            # Call internal build method
            esp8266_result = self._build_esp8266(
                board_id, env_config, build_flags, lib_deps, request
            )

            # Convert BuildResultESP8266 to BuildResult
            return BuildResult(
                success=esp8266_result.success,
                hex_path=esp8266_result.firmware_bin,
                elf_path=esp8266_result.firmware_elf,
                size_info=esp8266_result.size_info,
                build_time=esp8266_result.build_time,
                message=esp8266_result.message
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

    def _build_esp8266(
        self,
        board_id: str,
        env_config: dict,
        build_flags: List[str],
        lib_deps: List[str],
        request: "BuildParams",
    ) -> BuildResultESP8266:
        """
        Execute complete ESP8266 build process (internal implementation).

        Args:
            board_id: Board ID (e.g., nodemcuv2, d1_mini)
            env_config: Environment configuration dict
            build_flags: User build flags from platformio.ini
            lib_deps: Library dependencies from platformio.ini
            request: Build request with basic parameters

        Returns:
            BuildResultESP8266 with build status and output paths
        """
        start_time = time.time()

        # Extract from request
        project_dir = request.project_dir
        env_name = request.env_name
        verbose = request.verbose
        build_dir = request.build_dir

        try:
            # Get platform URL from env_config
            platform_url = env_config.get('platform')
            if not platform_url:
                return self._error_result(
                    start_time,
                    "No platform URL specified in platformio.ini"
                )

            # Resolve platform shorthand to actual download URL
            platform_url = self._resolve_platform_url(platform_url)

            # Print build profile banner
            from .build_profiles import print_profile_banner
            print_profile_banner(request.profile)

            # Initialize platform
            log_phase(3, 11, "Initializing ESP8266 platform...")

            platform = PlatformESP8266(self.cache, platform_url, show_progress=True)
            platform.ensure_platform()

            # Get board configuration
            board_json = platform.get_board_json(board_id)
            mcu = board_json.get("build", {}).get("mcu", "esp8266")

            log_detail(f"Board: {board_id}", verbose_only=True)
            log_detail(f"MCU: {mcu}", verbose_only=True)

            # Get required packages
            packages = platform.get_required_packages(mcu)

            # Initialize toolchain
            toolchain = self._setup_toolchain(packages, start_time, verbose)
            if toolchain is None:
                return self._error_result(
                    start_time,
                    "Failed to initialize toolchain"
                )

            # Initialize framework
            framework = self._setup_framework(packages, start_time, verbose)
            if framework is None:
                return self._error_result(
                    start_time,
                    "Failed to initialize framework"
                )

            # Ensure build directory exists
            build_dir.mkdir(parents=True, exist_ok=True)

            # Determine source directory for cache invalidation
            from ..config import PlatformIOConfig
            config_for_src_dir = PlatformIOConfig(project_dir / "platformio.ini")
            src_dir_override = config_for_src_dir.get_src_dir()
            source_dir = project_dir / src_dir_override if src_dir_override else project_dir

            # Check build state and invalidate cache if needed
            log_detail("Checking build configuration state...", verbose_only=True)

            state_tracker = BuildStateTracker(build_dir)
            needs_rebuild, reasons, current_state = state_tracker.check_invalidation(
                platformio_ini_path=project_dir / "platformio.ini",
                platform="esp8266",
                board=board_id,
                framework=env_config.get('framework', 'arduino'),
                toolchain_version=toolchain.version,
                framework_version=framework.version,
                platform_version=platform.version,
                build_flags=build_flags,
                lib_deps=lib_deps,
                source_dir=source_dir,
            )

            if needs_rebuild:
                log_detail("Build cache invalidated:", verbose_only=True)
                for reason in reasons:
                    log_detail(f"  - {reason}", indent=8, verbose_only=True)
                log_detail("Cleaning build artifacts...", verbose_only=True)
                # Clean build artifacts to force rebuild
                from .build_utils import safe_rmtree
                if build_dir.exists():
                    safe_rmtree(build_dir)
                # Recreate build directory
                build_dir.mkdir(parents=True, exist_ok=True)
            else:
                log_detail("Build configuration unchanged, using cached artifacts", verbose_only=True)

            # Initialize compilation executor
            from .compilation_executor import CompilationExecutor
            compilation_executor = CompilationExecutor(
                build_dir=build_dir,
                show_progress=verbose,
                cache=self.cache,
                mcu=mcu,
                framework_version=framework.version,
            )

            # Load platform configuration ONCE
            from .. import platform_configs
            platform_config = platform_configs.load_config(mcu)
            if platform_config is None:
                return self._error_result(
                    start_time,
                    f"No platform configuration found for {mcu}. Available: {platform_configs.list_available_configs()}"
                )

            # Extract variant and core from board config
            variant = board_json.get("build", {}).get("variant", "")
            core = board_json.get("build", {}).get("core", "esp8266")

            # Create full BuildContext with all configuration loaded once
            from .build_context import BuildContext
            context = BuildContext.from_request(
                request=request,
                platform=platform,
                toolchain=toolchain,
                mcu=mcu,
                framework_version=framework.version,
                compilation_executor=compilation_executor,
                cache=self.cache,
                # Consolidated fields
                framework=framework,
                board_id=board_id,
                board_config=board_json,
                platform_config=platform_config,
                variant=variant,
                core=core,
                user_build_flags=build_flags,
                env_config=env_config,
            )

            # Initialize compiler (uses BuildContext for all configuration)
            log_phase(5, 11, "Compiling Arduino core...")

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
                core_sources = self._get_core_sources(framework, compiler.core)
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
                log_detail(f"Compiled {len(core_obj_files)} core files")

            # Wait for all pending async compilation jobs to complete
            if hasattr(compiler, "wait_all_jobs"):
                compiler.wait_all_jobs()

            core_archive = compiler.create_core_archive(core_obj_files)

            log_detail(f"Compiled {len(core_obj_files)} core source files", verbose_only=True)

            # Handle library dependencies
            library_archives = []
            library_include_paths = []
            if lib_deps:
                log_phase(6, 11, "Processing library dependencies...")
                log_warning("ESP8266 library compilation not yet implemented, skipping libraries")

            # Add library include paths to compiler
            if library_include_paths:
                compiler.add_library_includes(library_include_paths)

            # Find and compile sketch
            sketch_obj_files = self._compile_sketch(project_dir, compiler, start_time, verbose, src_dir_override)
            if sketch_obj_files is None:
                search_dir = project_dir / src_dir_override if src_dir_override else project_dir
                return self._error_result(
                    start_time,
                    f"No .ino sketch file found in {search_dir}"
                )

            # Initialize linker
            log_phase(8, 11, "Linking firmware...")

            linker = ConfigurableLinker(context)

            # Link firmware
            firmware_elf = linker.link(sketch_obj_files, core_archive, library_archives=library_archives)

            # Generate binary
            log_phase(9, 11, "Generating firmware binary...")

            firmware_bin = linker.generate_bin(firmware_elf)

            # Get size information from ELF file
            size_info = linker.get_size_info(firmware_elf)

            build_time = time.time() - start_time

            if verbose:
                self._print_success(
                    build_time, firmware_elf, firmware_bin, size_info
                )

            # Save build state for future cache validation
            log_detail("Saving build state...", verbose_only=True)
            state_tracker.save_state(current_state)

            # Generate build_info.json
            build_info_generator = BuildInfoGenerator(build_dir)
            board_name = board_json.get("name", board_id)
            # Parse f_cpu from string (e.g., "80000000L" or "80000000") to int
            f_cpu_raw = board_json.get("build", {}).get("f_cpu", "0")
            f_cpu_int = int(str(f_cpu_raw).rstrip("L")) if f_cpu_raw else 0
            # Build toolchain_paths dict
            toolchain_paths = {
                "gcc": toolchain.get_gcc_path(),
                "gxx": toolchain.get_gxx_path(),
                "ar": toolchain.get_ar_path(),
                "objcopy": toolchain.get_objcopy_path(),
            }
            # Fallback flash settings from board JSON if not in env_config
            flash_mode_env = env_config.get("board_build.flash_mode")
            flash_mode_board = board_json.get("build", {}).get("flash_mode", "qio")
            flash_mode = flash_mode_env or flash_mode_board
            flash_size_env = env_config.get("board_build.flash_size")
            flash_size_board = board_json.get("upload", {}).get("flash_size", "4MB")
            flash_size = flash_size_env or flash_size_board

            build_info = build_info_generator.generate_esp32(
                env_name=env_name,
                board_id=board_id,
                board_name=board_name,
                mcu=mcu,
                f_cpu=f_cpu_int,
                build_time=build_time,
                elf_path=firmware_elf,
                bin_path=firmware_bin,
                size_info=size_info,
                build_flags=build_flags,
                lib_deps=lib_deps,
                toolchain_version=toolchain.version,
                toolchain_paths=toolchain_paths,
                framework_version=framework.version,
                core_path=framework.get_cores_dir(),
                bootloader_path=None,  # ESP8266 doesn't use bootloader
                partitions_path=None,  # ESP8266 doesn't use partition table
                application_offset="0x0",  # ESP8266 flashes at 0x0
                flash_mode=flash_mode,
                flash_size=flash_size,
            )
            build_info_generator.save(build_info)
            log_detail(f"Build info saved to {build_info_generator.build_info_path}", verbose_only=True)

            return BuildResultESP8266(
                success=True,
                firmware_bin=firmware_bin,
                firmware_elf=firmware_elf,
                size_info=size_info,
                build_time=build_time,
                message="Build successful (native ESP8266 build)"
            )

        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            build_time = time.time() - start_time
            import traceback
            error_trace = traceback.format_exc()
            return BuildResultESP8266(
                success=False,
                firmware_bin=None,
                firmware_elf=None,
                size_info=None,
                build_time=build_time,
                message=f"ESP8266 native build failed: {e}\n\n{error_trace}"
            )

    def _setup_toolchain(
        self,
        packages: dict,
        start_time: float,
        verbose: bool
    ) -> Optional['ToolchainESP8266']:
        """
        Initialize ESP8266 toolchain.

        Args:
            packages: Package URLs dictionary
            start_time: Build start time for error reporting
            verbose: Verbose output mode

        Returns:
            ToolchainESP8266 instance or None on failure
        """
        log_phase(4, 11, "Initializing ESP8266 toolchain...")

        toolchain_url = packages.get("toolchain")
        if not toolchain_url:
            return None

        toolchain = ToolchainESP8266(
            self.cache,
            toolchain_url,
            show_progress=True
        )
        toolchain.ensure_toolchain()
        return toolchain

    def _setup_framework(
        self,
        packages: dict,
        start_time: float,
        verbose: bool
    ) -> Optional[FrameworkESP8266]:
        """
        Initialize ESP8266 framework.

        Args:
            packages: Package URLs dictionary
            start_time: Build start time for error reporting
            verbose: Verbose output mode

        Returns:
            FrameworkESP8266 instance or None on failure
        """
        log_phase(5, 11, "Initializing ESP8266 framework...")

        framework_url = packages.get("framework")

        if not framework_url:
            return None

        framework = FrameworkESP8266(
            self.cache,
            framework_url,
            show_progress=True
        )
        framework.ensure_framework()
        return framework

    def _get_core_sources(self, framework: FrameworkESP8266, core: str) -> List[Path]:
        """
        Get list of core source files for progress tracking.

        Args:
            framework: Framework instance
            core: Core name (e.g., "esp8266")

        Returns:
            List of core source file paths
        """
        cores_dir = framework.get_cores_dir()

        # Find all C/C++ source files in cores directory
        c_files = list(cores_dir.glob("*.c"))
        cpp_files = list(cores_dir.glob("*.cpp"))

        return c_files + cpp_files

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
        log_phase(7, 11, "Compiling sketch...")

        # Determine source directory
        if src_dir_override:
            src_dir = project_dir / src_dir_override
            log_detail(f"Using source directory override: {src_dir_override}", verbose_only=True)
        else:
            src_dir = project_dir

        # Look for .ino files in the source directory
        sketch_files = list(src_dir.glob("*.ino"))
        if not sketch_files:
            return None

        sketch_path = sketch_files[0]
        sketch_obj_files = compiler.compile_sketch(sketch_path)

        log_detail(f"Compiled {len(sketch_obj_files)} sketch file(s)", verbose_only=True)

        return sketch_obj_files

    def _print_success(
        self,
        build_time: float,
        firmware_elf: Path,
        firmware_bin: Path,
        size_info: Optional[SizeInfo] = None
    ) -> None:
        """
        Print build success message.

        Args:
            build_time: Total build time
            firmware_elf: Path to firmware ELF
            firmware_bin: Path to firmware binary
            size_info: Optional size information to display
        """
        # Build success message
        message_lines = ["BUILD SUCCESSFUL!"]
        message_lines.append(f"Build time: {build_time:.2f}s")
        message_lines.append(f"Firmware ELF: {firmware_elf}")
        message_lines.append(f"Firmware BIN: {firmware_bin}")

        BannerFormatter.print_banner("\n".join(message_lines), width=60, center=False)

        # Print size information if available
        if size_info:
            print()
            from .build_utils import SizeInfoPrinter
            SizeInfoPrinter.print_size_info(size_info)
            print()

    def _error_result(self, start_time: float, message: str) -> BuildResultESP8266:
        """
        Create an error result.

        Args:
            start_time: Build start time
            message: Error message

        Returns:
            BuildResultESP8266 indicating failure
        """
        return BuildResultESP8266(
            success=False,
            firmware_bin=None,
            firmware_elf=None,
            size_info=None,
            build_time=time.time() - start_time,
            message=message
        )

    @staticmethod
    def _resolve_platform_url(platform_spec: str) -> str:
        """
        Resolve platform specification to actual download URL.

        PlatformIO supports several formats for specifying platforms:
        - Full URL: "https://github.com/.../platform-espressif8266.zip" -> used as-is
        - Shorthand: "platformio/espressif8266" -> resolved to stable release
        - Name only: "espressif8266" -> resolved to stable release

        Args:
            platform_spec: Platform specification from platformio.ini

        Returns:
            Actual download URL for the platform
        """
        # Default stable release URL for espressif8266
        DEFAULT_ESP8266_URL = "https://github.com/platformio/platform-espressif8266/releases/download/v4.2.1/platform-espressif8266.zip"

        # If it's already a proper URL, use it as-is
        if platform_spec.startswith("http://") or platform_spec.startswith("https://"):
            return platform_spec

        # Handle PlatformIO shorthand formats
        if platform_spec in ("platformio/espressif8266", "espressif8266"):
            log_detail(f"Resolving platform shorthand '{platform_spec}' to stable release")
            return DEFAULT_ESP8266_URL

        # For unknown formats, return as-is and let the download fail with a clear error
        log_warning(f"Unknown platform format: {platform_spec}, attempting to use as URL")
        return platform_spec
