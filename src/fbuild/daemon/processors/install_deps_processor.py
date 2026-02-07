"""
Install Dependencies Processor - Handles dependency installation operations.

This module implements the InstallDependenciesProcessor which downloads and
caches all dependencies (toolchain, platform, framework, libraries) without
performing actual compilation. Useful for:
- Pre-warming the cache before builds
- Ensuring dependencies are available offline
- Separating dependency installation from compilation
"""

import importlib
import logging
import sys
from pathlib import Path
from typing import TYPE_CHECKING

from fbuild.daemon.messages import OperationType
from fbuild.daemon.request_processor import RequestProcessor

if TYPE_CHECKING:
    from fbuild.daemon.daemon_context import DaemonContext
    from fbuild.daemon.messages import DaemonState, InstallDependenciesRequest
    from fbuild.packages.cache import Cache


class InstallDependenciesProcessor(RequestProcessor):
    """Processor for install dependencies requests.

    This processor handles downloading and caching of build dependencies
    without performing actual compilation. It:
    1. Reloads build modules to pick up code changes (for development)
    2. Detects platform type from platformio.ini
    3. Downloads and caches platform, toolchain, framework
    4. Installs library dependencies
    5. Returns success/failure based on installation result

    Example:
        >>> processor = InstallDependenciesProcessor()
        >>> success = processor.process_request(install_deps_request, daemon_context)
    """

    def get_operation_type(self) -> OperationType:
        """Return BUILD operation type (dependencies are part of build)."""
        return OperationType.BUILD

    def get_required_locks(self, request: "InstallDependenciesRequest", context: "DaemonContext") -> dict[str, str]:
        """Install dependencies requires only a project lock.

        Args:
            request: The install dependencies request
            context: The daemon context

        Returns:
            Dictionary with project lock requirement
        """
        return {"project": request.project_dir}

    def get_starting_state(self) -> "DaemonState":
        """Get the daemon state when operation starts."""
        from fbuild.daemon.messages import DaemonState

        return DaemonState.BUILDING

    def get_starting_message(self, request: "InstallDependenciesRequest") -> str:
        """Get the status message when operation starts."""
        return f"Installing dependencies for {request.environment}"

    def get_success_message(self, request: "InstallDependenciesRequest") -> str:
        """Get the status message on success."""
        return "Dependencies installed successfully"

    def get_failure_message(self, request: "InstallDependenciesRequest") -> str:
        """Get the status message on failure."""
        return "Dependency installation failed"

    def execute_operation(self, request: "InstallDependenciesRequest", context: "DaemonContext") -> bool:
        """Execute the dependency installation operation.

        This installs all build dependencies without compiling:
        - Platform package
        - Toolchain (compiler, linker, etc.)
        - Framework (Arduino core, etc.)
        - Library dependencies from lib_deps

        Args:
            request: The install dependencies request
            context: The daemon context with all subsystems

        Returns:
            True if all dependencies installed successfully, False otherwise
        """
        logging.info(f"Installing dependencies for project: {request.project_dir}")

        # Reload build modules to pick up code changes
        self._reload_build_modules()

        # Detect platform type from platformio.ini
        try:
            from fbuild.config.ini_parser import PlatformIOConfig

            project_path = Path(request.project_dir)
            ini_path = project_path / "platformio.ini"

            if not ini_path.exists():
                logging.error(f"platformio.ini not found at {ini_path}")
                return False

            config = PlatformIOConfig(ini_path)
            env_config = config.get_env_config(request.environment)
            platform = env_config.get("platform", "").lower()
            board_id = env_config.get("board", "")
            lib_deps = config.get_lib_deps(request.environment)

            logging.info(f"Detected platform: {platform}, board: {board_id}")

        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise
        except Exception as e:
            logging.error(f"Failed to parse platformio.ini: {e}")
            return False

        # Normalize platform name
        platform_name = self._normalize_platform_name(platform)
        logging.info(f"Normalized platform: {platform_name}")

        # Install dependencies based on platform
        try:
            from fbuild.packages.cache import Cache

            cache = Cache(project_dir=Path(request.project_dir))

            if platform_name == "espressif32":
                return self._install_esp32_dependencies(cache, env_config, board_id, lib_deps, project_path, request.verbose)
            elif platform_name == "atmelavr":
                return self._install_avr_dependencies(cache, env_config, request.verbose)
            elif platform_name == "raspberrypi":
                return self._install_rp2040_dependencies(cache, env_config, request.verbose)
            elif platform_name == "ststm32":
                return self._install_stm32_dependencies(cache, env_config, request.verbose)
            else:
                logging.error(f"Unsupported platform: {platform_name}")
                return False

        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise
        except Exception as e:
            logging.error(f"Failed to install dependencies: {e}")
            import traceback

            logging.error(f"Traceback:\n{traceback.format_exc()}")
            return False

    def _normalize_platform_name(self, platform: str) -> str:
        """Normalize platform name from various formats.

        Args:
            platform: Raw platform string from platformio.ini

        Returns:
            Normalized platform name
        """
        if "platform-espressif32" in platform or "platformio/espressif32" in platform or platform == "espressif32":
            return "espressif32"
        elif "platform-atmelavr" in platform or "platformio/atmelavr" in platform or platform == "atmelavr":
            return "atmelavr"
        elif "platform-raspberrypi" in platform or "platformio/raspberrypi" in platform or platform == "raspberrypi":
            return "raspberrypi"
        elif "platform-ststm32" in platform or "platformio/ststm32" in platform or platform == "ststm32":
            return "ststm32"
        return platform

    def _install_esp32_dependencies(
        self,
        cache: "Cache",
        env_config: dict,
        board_id: str,
        lib_deps: list[str],
        project_dir: Path,
        verbose: bool,
    ) -> bool:
        """Install ESP32 platform dependencies.

        Args:
            cache: Cache instance for package management
            env_config: Environment configuration from platformio.ini
            board_id: Board identifier
            lib_deps: Library dependencies
            project_dir: Project directory
            verbose: Enable verbose output

        Returns:
            True if successful, False otherwise
        """
        from fbuild.packages.framework_esp32 import FrameworkESP32
        from fbuild.packages.library_manager_esp32 import LibraryManagerESP32
        from fbuild.packages.platform_esp32 import PlatformESP32
        from fbuild.packages.toolchain_esp32 import ToolchainESP32

        # Get platform URL
        platform_url = env_config.get("platform")
        if not platform_url:
            logging.error("No platform URL specified in platformio.ini")
            return False

        # Resolve platform shorthand to actual URL
        platform_url = self._resolve_platform_url(platform_url)

        # 1. Initialize platform
        logging.info("Installing ESP32 platform...")
        platform = PlatformESP32(cache, platform_url, show_progress=True)
        platform.ensure_platform()
        logging.info(f"Platform installed: version {platform.version}")

        # Get board configuration
        board_json = platform.get_board_json(board_id)
        mcu = board_json.get("build", {}).get("mcu", "esp32c6")

        # Get required packages (returns Dict[str, str] of package_name -> url)
        packages = platform.get_required_packages(mcu)

        # 2. Initialize toolchain
        logging.info("Installing ESP32 toolchain...")
        # Determine toolchain type based on MCU
        toolchain_type: str | None = None
        toolchain_url: str | None = None
        if "toolchain-riscv32-esp" in packages:
            toolchain_url = packages["toolchain-riscv32-esp"]
            toolchain_type = "riscv32-esp"
        elif "toolchain-xtensa-esp-elf" in packages:
            toolchain_url = packages["toolchain-xtensa-esp-elf"]
            toolchain_type = "xtensa-esp-elf"

        if toolchain_url and toolchain_type:
            toolchain = ToolchainESP32(cache, toolchain_url, toolchain_type, show_progress=True, mcu=mcu)
            toolchain.ensure_toolchain()
            logging.info(f"Toolchain installed: version {toolchain.version}")
        else:
            logging.warning("No toolchain package found for MCU")

        # 3. Initialize framework
        logging.info("Installing Arduino framework...")
        framework_url = packages.get("framework-arduinoespressif32")
        libs_url = packages.get("framework-arduinoespressif32-libs")
        if framework_url and libs_url:
            # Check for MCU-specific skeleton library
            mcu_suffix = mcu.replace("esp32", "")
            skeleton_lib_name = f"framework-arduino-{mcu_suffix}-skeleton-lib"
            skeleton_lib_url = packages.get(skeleton_lib_name)
            framework = FrameworkESP32(cache, framework_url, libs_url, skeleton_lib_url=skeleton_lib_url, show_progress=True)
            framework.ensure_framework()
            logging.info(f"Framework installed: version {framework.version}")
        else:
            logging.warning("No framework package found or missing libs URL")

        # 4. Install library dependencies
        if lib_deps:
            logging.info(f"Installing {len(lib_deps)} library dependencies...")
            from fbuild.packages.platformio_registry import LibrarySpec

            build_dir = cache.project_dir / ".fbuild" / "build" / board_id
            lib_manager = LibraryManagerESP32(build_dir, project_dir=project_dir)
            for lib_dep in lib_deps:
                logging.info(f"  Installing: {lib_dep}")
                try:
                    spec = LibrarySpec.parse(lib_dep)
                    lib_manager.download_library(spec, show_progress=True)
                except KeyboardInterrupt as ke:
                    from fbuild.interrupt_utils import (
                        handle_keyboard_interrupt_properly,
                    )

                    handle_keyboard_interrupt_properly(ke)
                    raise
                except Exception as e:
                    logging.error(f"Failed to install library {lib_dep}: {e}")
                    return False
            logging.info("All libraries installed successfully")
        else:
            logging.info("No library dependencies to install")

        return True

    def _install_avr_dependencies(
        self,
        cache: "Cache",
        env_config: dict,
        verbose: bool,
    ) -> bool:
        """Install AVR platform dependencies.

        Args:
            cache: Cache instance
            env_config: Environment configuration
            verbose: Enable verbose output

        Returns:
            True if successful, False otherwise
        """
        # AVR platform support - minimal implementation
        # TODO: Implement full AVR dependency installation
        logging.info("AVR dependency installation not yet fully implemented")
        logging.info("AVR builds will install dependencies on first build")
        return True

    def _install_rp2040_dependencies(
        self,
        cache: "Cache",
        env_config: dict,
        verbose: bool,
    ) -> bool:
        """Install RP2040 platform dependencies.

        Args:
            cache: Cache instance
            env_config: Environment configuration
            verbose: Enable verbose output

        Returns:
            True if successful, False otherwise
        """
        # RP2040 platform support - minimal implementation
        # TODO: Implement full RP2040 dependency installation
        logging.info("RP2040 dependency installation not yet fully implemented")
        logging.info("RP2040 builds will install dependencies on first build")
        return True

    def _install_stm32_dependencies(
        self,
        cache: "Cache",
        env_config: dict,
        verbose: bool,
    ) -> bool:
        """Install STM32 platform dependencies.

        Args:
            cache: Cache instance
            env_config: Environment configuration
            verbose: Enable verbose output

        Returns:
            True if successful, False otherwise
        """
        # STM32 platform support - minimal implementation
        # TODO: Implement full STM32 dependency installation
        logging.info("STM32 dependency installation not yet fully implemented")
        logging.info("STM32 builds will install dependencies on first build")
        return True

    def _resolve_platform_url(self, platform_url: str) -> str:
        """Resolve platform shorthand to actual download URL.

        Args:
            platform_url: Platform URL or shorthand from platformio.ini

        Returns:
            Resolved platform URL
        """
        # If it's already a full URL, return as-is
        if platform_url.startswith("http://") or platform_url.startswith("https://"):
            return platform_url

        # Handle PlatformIO shorthand formats - use default GitHub URLs
        # PlatformIO registry doesn't provide platform URLs, so we use known defaults
        platform_defaults = {
            "espressif32": "https://github.com/platformio/platform-espressif32.git",
            "atmelavr": "https://github.com/platformio/platform-atmelavr.git",
            "raspberrypi": "https://github.com/platformio/platform-raspberrypi.git",
            "ststm32": "https://github.com/platformio/platform-ststm32.git",
        }

        # Normalize the platform name
        normalized = platform_url.lower()
        if normalized.startswith("platformio/"):
            normalized = normalized.replace("platformio/", "")

        if normalized in platform_defaults:
            return platform_defaults[normalized]

        # If we can't resolve, return as-is and let the caller handle the error
        return platform_url

    def _reload_build_modules(self) -> None:
        """Reload build-related modules to pick up code changes.

        This is critical for development on Windows where daemon caching prevents
        testing code changes.
        """
        modules_to_reload = [
            # Package modules (reload first - no dependencies)
            "fbuild.packages.cache",
            "fbuild.packages.downloader",
            "fbuild.packages.archive_utils",
            "fbuild.packages.platformio_registry",
            "fbuild.packages.toolchain",
            "fbuild.packages.toolchain_esp32",
            "fbuild.packages.arduino_core",
            "fbuild.packages.framework_esp32",
            "fbuild.packages.platform_esp32",
            "fbuild.packages.library_manager",
            "fbuild.packages.library_manager_esp32",
            # Config system
            "fbuild.config.ini_parser",
            "fbuild.config.board_config",
            "fbuild.config.board_loader",
        ]

        reloaded_count = 0
        for module_name in modules_to_reload:
            try:
                if module_name in sys.modules:
                    importlib.reload(sys.modules[module_name])
                    reloaded_count += 1
                else:
                    __import__(module_name)
                    reloaded_count += 1
            except KeyboardInterrupt as ke:
                from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

                handle_keyboard_interrupt_properly(ke)
            except Exception as e:
                logging.warning(f"Failed to reload/import module {module_name}: {e}")

        if reloaded_count > 0:
            logging.info(f"Loaded/reloaded {reloaded_count} package modules")
