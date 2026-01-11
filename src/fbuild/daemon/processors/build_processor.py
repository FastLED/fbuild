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

        # Get fresh orchestrator class after module reload
        # Using direct import would use cached version
        try:
            orchestrator_class = getattr(sys.modules["fbuild.build.orchestrator_avr"], "BuildOrchestratorAVR")
        except (KeyError, AttributeError) as e:
            logging.error(f"Failed to get BuildOrchestratorAVR class: {e}")
            return False

        # Create orchestrator and execute build
        orchestrator = orchestrator_class(verbose=request.verbose)
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
