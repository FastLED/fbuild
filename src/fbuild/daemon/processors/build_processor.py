"""
Build Request Processor - Handles build operations.

This module implements the BuildRequestProcessor which executes build
operations for Arduino/ESP32 projects using the appropriate orchestrator.
"""

import contextvars
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


# Platform name patterns for normalization
# Each key is the normalized platform name, values are patterns to match
_PLATFORM_PATTERNS: dict[str, list[str]] = {
    "espressif32": ["platform-espressif32", "platformio/espressif32", "espressif32"],
    "atmelavr": ["platform-atmelavr", "platformio/atmelavr", "atmelavr"],
    "raspberrypi": ["platform-raspberrypi", "platformio/raspberrypi", "raspberrypi"],
    "ststm32": ["platform-ststm32", "platformio/ststm32", "ststm32"],
}

# Mapping from normalized platform name to orchestrator info
_PLATFORM_ORCHESTRATORS: dict[str, tuple[str, str]] = {
    "atmelavr": ("fbuild.build.orchestrator_avr", "BuildOrchestratorAVR"),
    "espressif32": ("fbuild.build.orchestrator_esp32", "OrchestratorESP32"),
    "raspberrypi": ("fbuild.build.orchestrator_rp2040", "OrchestratorRP2040"),
    "ststm32": ("fbuild.build.orchestrator_stm32", "OrchestratorSTM32"),
    "teensy": ("fbuild.build.orchestrator_teensy", "OrchestratorTeensy"),
}


def _normalize_platform(platform: str) -> str:
    """Normalize platform name from various formats.

    Handles:
    - URL formats: "https://.../platform-espressif32.zip" -> "espressif32"
    - PlatformIO format: "platformio/espressif32" -> "espressif32"
    - Direct names: "atmelavr", "espressif32", etc.

    Args:
        platform: Raw platform string from platformio.ini

    Returns:
        Normalized platform name
    """
    for normalized_name, patterns in _PLATFORM_PATTERNS.items():
        for pattern in patterns:
            if pattern in platform or platform == pattern:
                return normalized_name
    return platform  # Return as-is if no pattern matches


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

    def __init__(self) -> None:
        """Initialize the build processor."""
        super().__init__()
        self._last_error_message: str | None = None

    def get_failure_message(self, request: "BuildRequest") -> str:
        """Get the status message on failure.

        Returns the actual build error message if available, otherwise
        returns a generic failure message.

        Args:
            request: The request that failed

        Returns:
            Human-readable failure message with actual error details
        """
        if self._last_error_message:
            return f"Build failed: {self._last_error_message}"
        return "Build failed"

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
        """Execute the build operation with isolated output context.

        This is the core build logic extracted from the original
        process_build_request function. All boilerplate (locks, status
        updates, error handling) is handled by the base RequestProcessor.

        The build runs in an isolated context copy to ensure concurrent builds
        don't interfere with each other's output settings (timestamps, verbose
        flags, output files). This is critical for thread safety.

        Args:
            request: The build request containing project_dir, environment, etc.
            context: The daemon context with all subsystems

        Returns:
            True if build succeeded, False otherwise
        """
        # Run build in isolated context to prevent concurrent builds from
        # interfering with each other's output state
        ctx = contextvars.copy_context()
        return ctx.run(self._execute_operation_isolated, request, context)

    def _execute_operation_isolated(self, request: "BuildRequest", context: "DaemonContext") -> bool:
        """Execute build with isolated output context (internal implementation).

        This method runs within an isolated context created by execute_operation().
        All output.py contextvars (start_time, output_file, verbose) are isolated
        from other concurrent builds.

        Args:
            request: The build request containing project_dir, environment, etc.
            context: The daemon context with all subsystems

        Returns:
            True if build succeeded, False otherwise
        """
        logging.info(f"Building project: {request.project_dir}")

        # Clear any previous error message
        self._last_error_message = None

        # Reload build modules FIRST to pick up code changes
        # This is critical for development on Windows where daemon caching
        # prevents testing code changes
        # NOTE: With contextvars, the output context SURVIVES module reload!
        # The context is stored in the interpreter, not in the module.
        self._reload_build_modules()

        # CHECK: After module reload, before expensive platform init
        # This is a strategic cancellation check point - module reload is expensive,
        # so we check if the client has disconnected before proceeding
        from fbuild.daemon.cancellation import check_and_raise_if_cancelled

        check_and_raise_if_cancelled(context.cancellation_registry, request.request_id, request.caller_pid, "build")

        # Set up output file for streaming to client
        # Now safe to do after reload because context survives reload
        from fbuild.output import reset_timer, set_output_file

        output_file_path = Path(request.project_dir) / ".fbuild" / "build_output.txt"
        output_file_path.parent.mkdir(parents=True, exist_ok=True)

        # Clear output file at start to prevent stale output from previous builds
        output_file_path.write_text("", encoding="utf-8")

        output_file = None
        try:
            output_file = open(output_file_path, "a", encoding="utf-8")
            set_output_file(output_file)
            reset_timer()  # Fresh timestamps for this build

            result = self._execute_build(request, context)
            # Log after build completes
            logging.debug(f"Build execution completed with result={result}")
            return result
        finally:
            set_output_file(None)  # Always clean up
            if output_file is not None:
                output_file.close()
            # Explicit flush to ensure all output is visible
            import sys

            sys.stdout.flush()
            sys.stderr.flush()

    def _execute_build(self, request: "BuildRequest", context: "DaemonContext") -> bool:
        """Internal build execution logic.

        Args:
            request: The build request containing project_dir, environment, etc.
            context: The daemon context with all subsystems

        Returns:
            True if build succeeded, False otherwise
        """

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

        # Normalize platform name
        platform_name = _normalize_platform(platform)
        logging.info(f"Normalized platform: {platform_name}")

        # Get orchestrator info for the platform
        orchestrator_info = _PLATFORM_ORCHESTRATORS.get(platform_name)
        if not orchestrator_info:
            logging.error(f"Unsupported platform: {platform_name}")
            return False

        module_name, class_name = orchestrator_info

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

        # Get compilation queue from daemon context
        compilation_queue = context.compilation_queue

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
            jobs=request.jobs,
            queue=compilation_queue,
        )

        if not build_result.success:
            logging.error(f"Build failed: {build_result.message}")
            self._last_error_message = build_result.message
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
            "fbuild.packages.header_trampoline_cache",  # CRITICAL: Must reload trampoline cache
            "fbuild.packages.cache",
            "fbuild.packages.downloader",
            "fbuild.packages.archive_utils",
            "fbuild.packages.platformio_registry",
            "fbuild.packages.toolchain",
            "fbuild.packages.toolchain_esp32",
            "fbuild.packages.toolchain_teensy",
            "fbuild.packages.toolchain_rp2040",
            "fbuild.packages.toolchain_stm32",
            "fbuild.packages.arduino_core",
            "fbuild.packages.framework_esp32",
            "fbuild.packages.framework_teensy",
            "fbuild.packages.framework_rp2040",
            "fbuild.packages.framework_stm32",
            "fbuild.packages.platform_esp32",
            "fbuild.packages.platform_teensy",
            "fbuild.packages.platform_rp2040",
            "fbuild.packages.platform_stm32",
            "fbuild.packages.library_manager",
            "fbuild.packages.library_manager_esp32",
            # Config system (reload early - needed to detect platform type)
            "fbuild.config.ini_parser",
            "fbuild.config.board_config",
            "fbuild.config.board_loader",
            # Build system (reload second - depends on packages)
            "fbuild.build.archive_creator",
            "fbuild.build.flag_builder",
            "fbuild.build.compiler",
            "fbuild.build.configurable_compiler",
            "fbuild.build.linker",
            "fbuild.build.configurable_linker",
            "fbuild.build.source_scanner",
            "fbuild.build.compilation_executor",
            "fbuild.build.build_state",
            "fbuild.build.build_info_generator",
            "fbuild.build.build_utils",
            # Orchestrators (reload third - depends on build system)
            "fbuild.build.orchestrator",
            "fbuild.build.orchestrator_avr",
            "fbuild.build.orchestrator_esp32",
            "fbuild.build.orchestrator_teensy",
            "fbuild.build.orchestrator_rp2040",
            "fbuild.build.orchestrator_stm32",
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
