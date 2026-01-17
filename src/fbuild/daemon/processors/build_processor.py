"""
Build Request Processor - Handles build operations.

This module implements the BuildRequestProcessor which executes build
operations for Arduino/ESP32 projects using the appropriate orchestrator.
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
    from fbuild.daemon.messages import BuildRequest


class BuildRequestProcessor(RequestProcessor):
    """Processor for build requests.

    This processor handles compilation of Arduino/ESP32 projects. It:
    1. Reloads build modules to pick up code changes (for development)
    2. Creates the appropriate orchestrator (AVR or ESP32)
    3. Executes the build with the specified settings
    4. Returns success/failure based on build result

    Example:
        >>> processor = BuildRequestProcessor()
        >>> success = processor.process_request(build_request, daemon_context)
    """

    def get_operation_type(self) -> OperationType:
        """Return BUILD operation type."""
        return OperationType.BUILD

    def get_required_locks(self, request: "BuildRequest", context: "DaemonContext") -> dict[str, str]:
        """Build operations require only a project lock.

        Args:
            request: The build request
            context: The daemon context

        Returns:
            Dictionary with project lock requirement
        """
        return {"project": request.project_dir}

    def execute_operation(self, request: "BuildRequest", context: "DaemonContext") -> bool:
        """Execute the build operation.

        This is the core build logic extracted from the original
        process_build_request function. All boilerplate (locks, status
        updates, error handling) is handled by the base RequestProcessor.

        Args:
            request: The build request containing project_dir, environment, etc.
            context: The daemon context with all subsystems

        Returns:
            True if build succeeded, False otherwise
        """
        logging.info(f"Building project: {request.project_dir}")

        # Reload build modules to pick up code changes
        # This is critical for development on Windows where daemon caching
        # prevents testing code changes
        self._reload_build_modules()

        # Detect platform type from platformio.ini to select appropriate orchestrator
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

            logging.info(f"Detected platform: {platform}")

        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            logging.error(f"Failed to parse platformio.ini: {e}")
            return False

        # Normalize platform name (handle both direct names and URLs)
        # URLs like "https://.../platform-espressif32.zip" -> "espressif32"
        # URLs like "https://.../platform-atmelavr.zip" -> "atmelavr"
        platform_name = platform
        if "platform-espressif32" in platform:
            platform_name = "espressif32"
        elif "platform-atmelavr" in platform or platform == "atmelavr":
            platform_name = "atmelavr"

        logging.info(f"Normalized platform: {platform_name}")

        # Select orchestrator based on platform
        if platform_name == "atmelavr":
            module_name = "fbuild.build.orchestrator_avr"
            class_name = "BuildOrchestratorAVR"
        elif platform_name == "espressif32":
            module_name = "fbuild.build.orchestrator_esp32"
            class_name = "OrchestratorESP32"
        else:
            logging.error(f"Unsupported platform: {platform_name}")
            return False

        # Get fresh orchestrator class after module reload
        # Using direct import would use cached version
        try:
            orchestrator_class = getattr(sys.modules[module_name], class_name)
        except (KeyError, AttributeError) as e:
            logging.error(f"Failed to get {class_name} from {module_name}: {e}")
            return False

        # Create orchestrator and execute build
        # Create a Cache instance for package management
        from fbuild.packages.cache import Cache

        cache = Cache(project_dir=Path(request.project_dir))

        # Initialize orchestrator with cache (ESP32 requires it, AVR accepts it)
        logging.debug(f"[BUILD_PROCESSOR] Initializing {class_name} with cache={cache}, verbose={request.verbose}")
        logging.debug(f"[BUILD_PROCESSOR] orchestrator_class={orchestrator_class}, module={module_name}")
        orchestrator = orchestrator_class(cache=cache, verbose=request.verbose)
        logging.debug(f"[BUILD_PROCESSOR] orchestrator created successfully: {orchestrator}")
        build_result = orchestrator.build(
            project_dir=Path(request.project_dir),
            env_name=request.environment,
            clean=request.clean_build,
            verbose=request.verbose,
        )

        if not build_result.success:
            logging.error(f"Build failed: {build_result.message}")
            return False

        logging.info("Build completed successfully")
        return True

    def _reload_build_modules(self) -> None:
        """Reload build-related modules to pick up code changes.

        This is critical for development on Windows where daemon caching prevents
        testing code changes. Reloads key modules that are frequently modified.

        Order matters: reload dependencies first, then modules that import them.
        """
        modules_to_reload = [
            # Core utilities and packages (reload first - no dependencies)
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
            # Config system (reload early - needed to detect platform type)
            "fbuild.config.ini_parser",
            "fbuild.config.board_config",
            "fbuild.config.board_loader",
            # Build system (reload second - depends on packages)
            "fbuild.build.archive_creator",
            "fbuild.build.compiler",
            "fbuild.build.configurable_compiler",
            "fbuild.build.linker",
            "fbuild.build.configurable_linker",
            "fbuild.build.source_scanner",
            "fbuild.build.compilation_executor",
            # Orchestrators (reload third - depends on build system)
            "fbuild.build.orchestrator",
            "fbuild.build.orchestrator_avr",
            "fbuild.build.orchestrator_esp32",
            # Daemon processors (reload to pick up processor code changes)
            "fbuild.daemon.processors.build_processor",
            # Deploy and monitor (reload with build system)
            "fbuild.deploy.deployer",
            "fbuild.deploy.deployer_esp32",
            "fbuild.deploy.monitor",
            # Top-level module packages (reload last to update __init__.py imports)
            "fbuild.build",
            "fbuild.deploy",
        ]

        reloaded_count = 0
        for module_name in modules_to_reload:
            try:
                if module_name in sys.modules:
                    # Module already loaded - reload it to pick up changes
                    importlib.reload(sys.modules[module_name])
                    reloaded_count += 1
                else:
                    # Module not loaded yet - import it for the first time
                    __import__(module_name)
                    reloaded_count += 1
            except KeyboardInterrupt as ke:
                from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

                handle_keyboard_interrupt_properly(ke)
            except Exception as e:
                logging.warning(f"Failed to reload/import module {module_name}: {e}")

        if reloaded_count > 0:
            logging.info(f"Loaded/reloaded {reloaded_count} build modules")
