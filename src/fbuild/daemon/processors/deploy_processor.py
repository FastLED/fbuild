"""
Deploy Request Processor - Handles build + deploy operations.

This module implements the DeployRequestProcessor which executes build and
deployment operations for Arduino/ESP32 projects. It coordinates building
the firmware and then uploading it to the target device.

Enhanced in Iteration 2 with:
- FirmwareLedger integration to skip re-upload if firmware is unchanged
- Source and build flags hash tracking
- ConfigurationLockManager for centralized locking
"""

import logging
import sys
import time
from pathlib import Path
from typing import TYPE_CHECKING

from fbuild.daemon.firmware_ledger import (
    compute_build_flags_hash,
    compute_firmware_hash,
    compute_source_hash,
)
from fbuild.daemon.messages import DaemonState, MonitorRequest, OperationType
from fbuild.daemon.port_state_manager import PortState
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

    def __init__(self) -> None:
        """Initialize the deploy processor."""
        super().__init__()
        self._last_error_message: str | None = None

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
        """Get the failure status message.

        Returns the actual deploy error message if available, otherwise
        returns a generic failure message.

        Args:
            request: The request that failed

        Returns:
            Human-readable failure message with actual error details
        """
        if self._last_error_message:
            return f"Deploy failed: {self._last_error_message}"
        return "Deploy failed"

    def execute_operation(self, request: "DeployRequest", context: "DaemonContext") -> bool:
        """Execute the build + deploy operation.

        This is the core deploy logic extracted from the original
        process_deploy_request function. All boilerplate (locks, status
        updates, error handling) is handled by the base RequestProcessor.

        The operation has three phases:
        1. Check: See if firmware is already deployed (skip redeploy if unchanged)
        2. Build: Compile the firmware (can be skipped via skip_build flag)
        3. Deploy: Upload the firmware to device

        If monitor_after is requested, the processor will coordinate
        transitioning to monitoring after successful deployment.

        Args:
            request: The deploy request containing project_dir, environment, etc.
            context: The daemon context with all subsystems

        Returns:
            True if deploy succeeded, False otherwise
        """
        # Clear any previous error message
        self._last_error_message = None

        # Notify API monitors of impending preemption (if port is known)
        if request.port:
            self._notify_api_monitors_preemption(request.port, context)

        # Phase 0: Check if we can skip deployment using firmware ledger
        # Only check ledger if skip_build is not explicitly set (normal deploy flow)
        skip_deploy = False
        source_hash = ""
        build_flags_hash = ""

        if not request.skip_build:
            skip_deploy, source_hash, build_flags_hash = self._check_firmware_ledger(request, context)

            if skip_deploy and request.port:
                logging.info(f"Firmware unchanged, skipping build and deploy for {request.port}")

                # IMPORTANT: Even when skipping deploy, we must reset the device to release
                # the USB-CDC port. Without this, the port can get stuck in a locked state
                # on Windows because esptool's RTS/DTR reset sequence never runs.
                self._reset_device_port(request.port)

                # Update status to indicate skip
                self._update_status(
                    context,
                    DaemonState.COMPLETED,
                    "Firmware unchanged, skipping deploy",
                    request=request,
                    operation_in_progress=False,
                )
                # If monitoring requested, still start it (firmware is already there)
                if request.monitor_after:
                    self._start_monitoring(request, request.port, context)
                return True

        # Phase 1: Build firmware (skip if skip_build flag is set)
        if request.skip_build:
            logging.info(f"Skipping build phase (upload-only mode) for {request.project_dir}")
        else:
            logging.info(f"Building project: {request.project_dir}")
            build_result = self._build_firmware(request, context)
            if not build_result:
                return False

        # Phase 2: Deploy firmware
        logging.info(f"Deploying to {request.port if request.port else 'auto-detected port'}")
        used_port = self._deploy_firmware(request, context)
        if not used_port:
            return False

        # Phase 2.5: Record deployment in firmware ledger
        self._record_deployment(request, used_port, source_hash, build_flags_hash, context)

        # Phase 2.6: Clear preemption notification (deploy complete)
        if used_port:
            self._clear_api_monitor_preemption(used_port, context)

        # Phase 3: Optional monitoring or release port state
        if request.monitor_after and used_port:
            # _start_monitoring handles port state release when monitoring completes
            self._start_monitoring(request, used_port, context)
        else:
            # No monitoring requested - release port state now
            if used_port:
                context.port_state_manager.release_port(used_port)

        logging.info("Deploy completed successfully")
        return True

    def _check_firmware_ledger(self, request: "DeployRequest", context: "DaemonContext") -> tuple[bool, str, str]:
        """Check if firmware is already deployed and unchanged.

        Uses the firmware ledger to determine if we can skip the build and deploy.
        This is a major optimization when the same firmware is deployed multiple
        times without changes.

        Args:
            request: The deploy request
            context: The daemon context

        Returns:
            Tuple of (can_skip, source_hash, build_flags_hash)
        """
        if not request.port:
            # Can't check without a known port
            return False, "", ""

        try:
            project_path = Path(request.project_dir)

            # Get source files to hash
            source_files = self._get_source_files(project_path)
            if not source_files:
                logging.debug("No source files found for hashing")
                return False, "", ""

            # Compute hashes
            source_hash = compute_source_hash(source_files)
            build_flags_hash = compute_build_flags_hash(self._get_build_flags(project_path, request.environment))

            # Check if redeploy is needed
            needs_redeploy = context.firmware_ledger.needs_redeploy(
                port=request.port,
                source_hash=source_hash,
                build_flags_hash=build_flags_hash,
            )

            if not needs_redeploy:
                logging.info(f"Firmware ledger indicates no changes for {request.port}")
                return True, source_hash, build_flags_hash

            logging.debug("Source or build flags changed, redeploy needed")
            return False, source_hash, build_flags_hash

        except KeyboardInterrupt:
            raise
        except Exception as e:
            logging.warning(f"Error checking firmware ledger: {e}")
            return False, "", ""

    def _reset_device_port(self, port: str) -> bool:
        """Reset device on port using esptool's RTS/DTR sequence.

        This ensures the USB-CDC port is properly released even when we skip
        the firmware upload. Without this, Windows can leave the port in a
        locked state because esptool's hardware reset sequence never runs.

        Args:
            port: Serial port to reset (e.g., "COM13", "/dev/ttyUSB0")

        Returns:
            True if reset succeeded, False otherwise (non-fatal)
        """
        try:
            from fbuild.deploy.esptool_utils import reset_esp32_device

            logging.info(f"Resetting device on {port} to release USB-CDC port")
            success = reset_esp32_device(port, chip="auto", verbose=False)
            if success:
                logging.info(f"Device on {port} reset successfully")
            else:
                logging.warning(f"Device reset on {port} failed (non-fatal)")
            return success
        except KeyboardInterrupt:
            raise
        except Exception as e:
            logging.warning(f"Error resetting device on {port}: {e} (non-fatal)")
            return False

    def _get_source_files(self, project_path: Path) -> list[Path]:
        """Get list of source files in the project.

        Args:
            project_path: Path to the project directory

        Returns:
            List of source file paths
        """
        source_extensions = {".c", ".cpp", ".h", ".hpp", ".ino", ".S"}
        source_files = []

        # Check standard source directories
        src_dirs = [
            project_path / "src",
            project_path / "include",
            project_path / "lib",
        ]

        # Also check for .ino files in project root
        for f in project_path.glob("*.ino"):
            source_files.append(f)

        for src_dir in src_dirs:
            if src_dir.exists():
                for ext in source_extensions:
                    source_files.extend(src_dir.rglob(f"*{ext}"))

        return source_files

    def _get_build_flags(self, project_path: Path, environment: str) -> list[str]:
        """Get build flags from platformio.ini.

        Args:
            project_path: Path to the project directory
            environment: Build environment name

        Returns:
            List of build flags
        """
        try:
            from fbuild.config.ini_parser import PlatformIOConfig

            ini_path = project_path / "platformio.ini"
            if not ini_path.exists():
                return []

            config = PlatformIOConfig(ini_path)
            env_config = config.get_env_config(environment)
            build_flags = env_config.get("build_flags", "")

            return build_flags.split() if build_flags else []
        except KeyboardInterrupt:
            raise
        except Exception as e:
            logging.warning(f"Error reading build flags: {e}")
            return []

    def _record_deployment(
        self,
        request: "DeployRequest",
        port: str,
        source_hash: str,
        build_flags_hash: str,
        context: "DaemonContext",
    ) -> None:
        """Record successful deployment in firmware ledger.

        Args:
            request: The deploy request
            port: Port the firmware was deployed to
            source_hash: Hash of source files
            build_flags_hash: Hash of build flags
            context: The daemon context
        """
        try:
            project_path = Path(request.project_dir)

            # Find the firmware file
            firmware_path = self._find_firmware_path(project_path, request.environment)
            if not firmware_path:
                logging.warning("Could not find firmware file for ledger recording")
                return

            # Compute firmware hash
            firmware_hash = compute_firmware_hash(firmware_path)

            # Record in ledger
            context.firmware_ledger.record_deployment(
                port=port,
                firmware_hash=firmware_hash,
                source_hash=source_hash,
                project_dir=str(project_path),
                environment=request.environment,
                build_flags_hash=build_flags_hash,
            )
            logging.info(f"Recorded deployment in firmware ledger for {port}")

        except KeyboardInterrupt:
            raise
        except Exception as e:
            logging.warning(f"Error recording deployment in ledger: {e}")

    def _find_firmware_path(self, project_path: Path, environment: str) -> Path | None:
        """Find the firmware file for the given environment.

        Args:
            project_path: Path to the project directory
            environment: Build environment name

        Returns:
            Path to firmware file, or None if not found
        """
        # Check common firmware locations
        build_dir = project_path / ".pio" / "build" / environment
        if not build_dir.exists():
            build_dir = project_path / ".fbuild" / "build" / environment

        if not build_dir.exists():
            return None

        # Look for firmware files (prefer .bin, then .hex, then .elf)
        for ext in [".bin", ".hex", ".elf"]:
            for firmware_file in build_dir.glob(f"*{ext}"):
                return firmware_file

        return None

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
        # "raspberrypi" or "platform-raspberrypi" -> "raspberrypi"
        platform_name = platform
        if "platform-espressif32" in platform:
            platform_name = "espressif32"
        elif "platform-atmelavr" in platform or platform == "atmelavr":
            platform_name = "atmelavr"
        elif "platform-raspberrypi" in platform or platform == "raspberrypi":
            platform_name = "raspberrypi"

        logging.info(f"Normalized platform: {platform_name}")

        # Select orchestrator based on platform
        if platform_name == "atmelavr":
            module_name = "fbuild.build.orchestrator_avr"
            class_name = "BuildOrchestratorAVR"
        elif platform_name == "espressif32":
            module_name = "fbuild.build.orchestrator_esp32"
            class_name = "OrchestratorESP32"
        elif platform_name == "raspberrypi":
            module_name = "fbuild.build.orchestrator_rp2040"
            class_name = "OrchestratorRP2040"
        else:
            logging.error(f"Unsupported platform: {platform_name}")
            return False

        # Get fresh orchestrator class after module reload
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
        orchestrator = orchestrator_class(cache=cache, verbose=False)
        build_result = orchestrator.build(
            project_dir=Path(request.project_dir),
            env_name=request.environment,
            clean=request.clean_build,
            verbose=False,
        )

        if not build_result.success:
            logging.error(f"Build failed: {build_result.message}")
            # Store error message for get_failure_message()
            self._last_error_message = f"Build phase: {build_result.message}"
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

        # Import and get deployer class (explicit import ensures module is loaded)
        try:
            from fbuild.deploy.deployer_esp32 import ESP32Deployer

            deployer_class = ESP32Deployer
        except ImportError as e:
            logging.error(f"Failed to import ESP32Deployer: {e}")
            return None

        # Track port state as UPLOADING before deployment starts
        used_port = request.port
        if used_port:
            context.port_state_manager.acquire_port(
                port=used_port,
                state=PortState.UPLOADING,
                client_pid=request.caller_pid,
                project_dir=request.project_dir,
                environment=request.environment,
                operation_id=request.request_id,
            )

        try:
            # Execute deploy
            deployer = deployer_class(verbose=False)
            deploy_result = deployer.deploy(
                project_dir=Path(request.project_dir),
                env_name=request.environment,
                port=request.port,
            )

            if not deploy_result.success:
                logging.error(f"Deploy failed: {deploy_result.message}")
                # Store error message for get_failure_message()
                self._last_error_message = deploy_result.message
                self._update_status(
                    context,
                    DaemonState.FAILED,
                    f"Deploy failed: {deploy_result.message}",
                    request=request,
                    exit_code=1,
                    operation_in_progress=False,
                )
                # Release port state on failure
                if used_port:
                    context.port_state_manager.release_port(used_port)
                return None

            # Update used_port with actual port if auto-detected
            actual_port = deploy_result.port if deploy_result.port else request.port

            # If port changed (auto-detected), update port state tracking
            if actual_port and actual_port != used_port:
                # Release old port state if we tracked one
                if used_port:
                    context.port_state_manager.release_port(used_port)
                # Track the actual port used
                context.port_state_manager.acquire_port(
                    port=actual_port,
                    state=PortState.UPLOADING,
                    client_pid=request.caller_pid,
                    project_dir=request.project_dir,
                    environment=request.environment,
                    operation_id=request.request_id,
                )

            # Return the port that was actually used
            return actual_port

        except KeyboardInterrupt:
            logging.warning("Deploy interrupted by user")
            # Release port state on interruption
            if used_port:
                context.port_state_manager.release_port(used_port)
            raise
        except Exception as e:
            logging.error(f"Deploy exception: {e}")
            # Release port state on exception
            if used_port:
                context.port_state_manager.release_port(used_port)
            raise

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

        # Transition port state to MONITORING
        context.port_state_manager.update_state(port, PortState.MONITORING)

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
            show_timestamp=request.monitor_show_timestamp,
            request_id=request.request_id,
        )

        try:
            # Import and use MonitorRequestProcessor to handle monitoring
            # This will be imported at runtime to avoid circular dependencies
            # Give Windows USB-CDC driver time to release port after esptool finishes
            # and allow device to complete its post-deployment reboot
            # Without this delay, the monitor may try to open the port while the device
            # is still rebooting from deployment, causing "port busy" errors
            import time

            from fbuild.daemon.processors.monitor_processor import (
                MonitorRequestProcessor,
            )

            time.sleep(2.0)  # 2 second delay for device reboot and driver cleanup

            monitor_processor = MonitorRequestProcessor()
            # Note: This will block until monitoring completes
            # The locks will be released by the base class after execute_operation returns
            monitor_processor.process_request(monitor_request, context)
        finally:
            # Release port state when monitoring completes
            context.port_state_manager.release_port(port)

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
            # Deploy utilities (reload with build system)
            "fbuild.deploy.esptool_utils",
            "fbuild.deploy.serial_utils",
            "fbuild.deploy.docker_utils",
            "fbuild.deploy.platform_utils",
            # Deploy core (reload after utilities)
            "fbuild.deploy.deployer",
            "fbuild.deploy.deployer_esp32",
            "fbuild.deploy.monitor",
            "fbuild.deploy.qemu_runner",
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

    def _notify_api_monitors_preemption(self, port: str, context: "DaemonContext") -> None:
        """Notify API monitors that deploy is about to preempt them.

        Writes a preemption notification file that SerialMonitor clients can
        detect. Clients with auto_reconnect=True will pause and wait for deploy
        to complete.

        CRITICAL: Also FORCIBLY CLOSES any active serial monitor sessions on this port.
        The notification file alone is not enough - we must actually close the pyserial
        connection, otherwise esptool will get PermissionError(13) when trying to open
        the port for firmware upload.

        Args:
            port: Port that will be preempted
            context: Daemon context
        """
        try:
            import json

            from fbuild.daemon.paths import DAEMON_DIR

            # STEP 1: Force-close any active serial monitor sessions on this port
            # This is critical to release the pyserial handle before esptool attempts upload
            with context.shared_serial_manager._lock:
                if port in context.shared_serial_manager._sessions:
                    logging.info(f"[DeployPreemption] Force-closing active serial session on {port}")
                    context.shared_serial_manager._close_port_internal(port)
                    logging.info(f"[DeployPreemption] Serial session closed on {port}")

            # STEP 2: Write notification file for API clients
            preempt_file = DAEMON_DIR / f"serial_monitor_preempt_{port}.json"
            notification = {
                "port": port,
                "preempted_at": time.time(),
                "preempted_by": "deploy_operation",
            }

            # Write notification atomically
            temp_file = preempt_file.with_suffix(".tmp")
            with open(temp_file, "w") as f:
                json.dump(notification, f, indent=2)
            temp_file.replace(preempt_file)

            logging.info(f"[DeployPreemption] Notified API monitors of preemption on {port}")

        except KeyboardInterrupt:
            raise
        except Exception as e:
            logging.warning(f"[DeployPreemption] Failed to notify API monitors: {e}")

    def _clear_api_monitor_preemption(self, port: str, context: "DaemonContext") -> None:
        """Clear preemption notification after deploy completes.

        Deletes the preemption file, signaling to SerialMonitor clients that
        they can reconnect.

        Args:
            port: Port that was preempted
            context: Daemon context
        """
        try:
            from fbuild.daemon.paths import DAEMON_DIR

            preempt_file = DAEMON_DIR / f"serial_monitor_preempt_{port}.json"
            if preempt_file.exists():
                preempt_file.unlink()
                logging.info(f"[DeployPreemption] Cleared preemption notification for {port}")

        except KeyboardInterrupt:
            raise
        except Exception as e:
            logging.warning(f"[DeployPreemption] Failed to clear preemption notification: {e}")
