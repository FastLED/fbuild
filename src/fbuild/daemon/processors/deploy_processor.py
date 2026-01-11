"""
Deploy Request Processor - Handles build + deploy operations.

This module implements the DeployRequestProcessor which executes build and
deployment operations for Arduino/ESP32 projects. It coordinates building
the firmware and then uploading it to the target device.
"""

import logging
import sys
from pathlib import Path
from typing import TYPE_CHECKING

from fbuild.daemon.messages import DaemonState, MonitorRequest, OperationType
from fbuild.daemon.request_processor import RequestProcessor

if TYPE_CHECKING:
    from fbuild.daemon.daemon_context import DaemonContext
    from fbuild.daemon.messages import DeployRequest


class DeployRequestProcessor(RequestProcessor):
    """Processor for deploy requests.

    This processor handles building and deploying Arduino/ESP32 projects. It:
    1. Reloads build modules to pick up code changes (for development)
    2. Builds the firmware using the appropriate orchestrator
    3. Deploys the firmware to the target device
    4. Optionally starts monitoring after successful deployment

    The processor coordinates two major phases (build + deploy) and handles
    the complexity of transitioning to monitoring if requested.

    Example:
        >>> processor = DeployRequestProcessor()
        >>> success = processor.process_request(deploy_request, daemon_context)
    """

    def get_operation_type(self) -> OperationType:
        """Return DEPLOY operation type."""
        return OperationType.DEPLOY

    def get_required_locks(self, request: "DeployRequest", context: "DaemonContext") -> dict[str, str]:
        """Deploy operations require both project and port locks.

        Args:
            request: The deploy request
            context: The daemon context

        Returns:
            Dictionary with project and port lock requirements
        """
        locks = {"project": request.project_dir}
        if request.port:
            locks["port"] = request.port
        return locks

    def get_starting_state(self) -> DaemonState:
        """Deploy starts in DEPLOYING state."""
        return DaemonState.DEPLOYING

    def get_starting_message(self, request: "DeployRequest") -> str:
        """Get the starting status message."""
        return f"Deploying {request.environment}"

    def get_success_message(self, request: "DeployRequest") -> str:
        """Get the success status message."""
        return "Deploy successful"

    def get_failure_message(self, request: "DeployRequest") -> str:
        """Get the failure status message."""
        return "Deploy failed"

    def execute_operation(self, request: "DeployRequest", context: "DaemonContext") -> bool:
        """Execute the build + deploy operation.

        This is the core deploy logic extracted from the original
        process_deploy_request function. All boilerplate (locks, status
        updates, error handling) is handled by the base RequestProcessor.

        The operation has two phases:
        1. Build: Compile the firmware
        2. Deploy: Upload the firmware to device

        If monitor_after is requested, the processor will coordinate
        transitioning to monitoring after successful deployment.

        Args:
            request: The deploy request containing project_dir, environment, etc.
            context: The daemon context with all subsystems

        Returns:
            True if deploy succeeded, False otherwise
        """
        # Phase 1: Build firmware
        logging.info(f"Building project: {request.project_dir}")
        if not self._build_firmware(request, context):
            return False

        # Phase 2: Deploy firmware
        logging.info(f"Deploying to {request.port if request.port else 'auto-detected port'}")
        used_port = self._deploy_firmware(request, context)
        if not used_port:
            return False

        # Phase 3: Optional monitoring
        if request.monitor_after and used_port:
            self._start_monitoring(request, used_port, context)

        logging.info("Deploy completed successfully")
        return True

    def _build_firmware(self, request: "DeployRequest", context: "DaemonContext") -> bool:
        """Build the firmware.

        Args:
            request: The deploy request
            context: The daemon context

        Returns:
            True if build succeeded, False otherwise
        """
        # Update status to building
        self._update_status(
            context,
            DaemonState.BUILDING,
            f"Building {request.environment}",
            request=request,
            operation_type=OperationType.BUILD_AND_DEPLOY,
        )

        # Reload build modules to pick up code changes
        self._reload_build_modules()

        # Get fresh orchestrator class after module reload
        try:
            orchestrator_class = getattr(sys.modules["fbuild.build.orchestrator_avr"], "BuildOrchestratorAVR")
        except (KeyError, AttributeError) as e:
            logging.error(f"Failed to get BuildOrchestratorAVR class: {e}")
            return False

        # Execute build
        orchestrator = orchestrator_class(verbose=False)
        build_result = orchestrator.build(
            project_dir=Path(request.project_dir),
            env_name=request.environment,
            clean=request.clean_build,
            verbose=False,
        )

        if not build_result.success:
            logging.error(f"Build failed: {build_result.message}")
            self._update_status(
                context,
                DaemonState.FAILED,
                f"Build failed: {build_result.message}",
                request=request,
                exit_code=1,
                operation_in_progress=False,
            )
            return False

        logging.info("Build completed successfully")
        return True

    def _deploy_firmware(self, request: "DeployRequest", context: "DaemonContext") -> str | None:
        """Deploy the firmware to the device.

        Args:
            request: The deploy request
            context: The daemon context

        Returns:
            The port that was used for deployment, or None if deployment failed
        """
        # Update status to deploying
        self._update_status(
            context,
            DaemonState.DEPLOYING,
            f"Deploying {request.environment}",
            request=request,
            operation_type=OperationType.DEPLOY,
        )

        # Get fresh deployer class after module reload
        try:
            deployer_class = getattr(sys.modules["fbuild.deploy.deployer_esp32"], "ESP32Deployer")
        except (KeyError, AttributeError) as e:
            logging.error(f"Failed to get ESP32Deployer class: {e}")
            return None

        # Execute deploy
        deployer = deployer_class(verbose=False)
        deploy_result = deployer.deploy(
            project_dir=Path(request.project_dir),
            env_name=request.environment,
            port=request.port,
        )

        if not deploy_result.success:
            logging.error(f"Deploy failed: {deploy_result.message}")
            self._update_status(
                context,
                DaemonState.FAILED,
                f"Deploy failed: {deploy_result.message}",
                request=request,
                exit_code=1,
                operation_in_progress=False,
            )
            return None

        # Return the port that was actually used
        return deploy_result.port if deploy_result.port else request.port

    def _start_monitoring(self, request: "DeployRequest", port: str, context: "DaemonContext") -> None:
        """Start monitoring after successful deployment.

        This creates a MonitorRequest and processes it immediately.
        Note: This is called while still holding locks, so we need to
        release them first by returning from execute_operation.

        For now, we'll just log that monitoring should start. The actual
        implementation of post-deploy monitoring will be handled in the
        daemon.py integration (Task 1.8).

        Args:
            request: The deploy request
            port: The port to monitor
            context: The daemon context
        """
        logging.info(f"Monitor after deploy requested for port {port}")

        # Update status to indicate transition to monitoring
        self._update_status(
            context,
            DaemonState.MONITORING,
            "Transitioning to monitor after deploy",
            request=request,
        )

        # Create monitor request
        monitor_request = MonitorRequest(
            project_dir=request.project_dir,
            environment=request.environment,
            port=port,
            baud_rate=None,  # Use config default
            halt_on_error=request.monitor_halt_on_error,
            halt_on_success=request.monitor_halt_on_success,
            expect=request.monitor_expect,
            timeout=request.monitor_timeout,
            caller_pid=request.caller_pid,
            caller_cwd=request.caller_cwd,
            request_id=request.request_id,
        )

        # Import and use MonitorRequestProcessor to handle monitoring
        # This will be imported at runtime to avoid circular dependencies
        from fbuild.daemon.processors.monitor_processor import MonitorRequestProcessor

        monitor_processor = MonitorRequestProcessor()
        # Note: This will block until monitoring completes
        # The locks will be released by the base class after execute_operation returns
        monitor_processor.process_request(monitor_request, context)

    def _reload_build_modules(self) -> None:
        """Reload build-related modules to pick up code changes.

        This is critical for development on Windows where daemon caching prevents
        testing code changes. Reloads key modules that are frequently modified.

        Order matters: reload dependencies first, then modules that import them.
        """
        import importlib

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
