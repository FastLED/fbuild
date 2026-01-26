"""fbuild - Modern embedded development tool."""

from dataclasses import dataclass
from pathlib import Path
from typing import Any

from fbuild.daemon.connection import DaemonConnection, connect_daemon

__version__ = "1.3.24"


def is_available() -> bool:
    """Check if fbuild is properly installed and functional.

    Returns:
        True if fbuild daemon client is available, False otherwise.
    """
    try:
        __import__("fbuild.daemon.client")
        return True
    except ImportError:
        return False


@dataclass
class BuildContext:
    """Configuration context for fbuild operations.

    Groups common parameters used across build, deploy, and install operations.
    Can be passed to Daemon methods instead of individual parameters.

    Example usage:
        import fbuild

        # Create a context for repeated operations
        ctx = fbuild.BuildContext(
            project_dir=Path("my_project"),
            environment="esp32dev",
            port="COM3",
            verbose=True
        )

        # Pre-install dependencies (toolchain, framework, libraries)
        fbuild.Daemon.install_dependencies(ctx)

        # Build using the context
        fbuild.Daemon.build(ctx)

        # Deploy using the context
        fbuild.Daemon.deploy(ctx)

    Attributes:
        project_dir: Path to project directory containing platformio.ini
        environment: Build environment name (e.g., 'esp32dev', 'esp32c6')
        port: Serial port for upload/monitor (auto-detect if None)
        clean_build: Whether to perform a clean build
        verbose: Enable verbose output
        timeout: Maximum wait time in seconds (default: 30 minutes)
    """

    project_dir: Path
    environment: str
    port: str | None = None
    clean_build: bool = False
    verbose: bool = False
    timeout: float = 1800


class Daemon:
    """Daemon management API for fbuild.

    Provides static methods to control the fbuild daemon which handles
    build, deploy, and monitor operations.

    Example usage:
        import fbuild

        # Option 1: Use BuildContext (recommended for repeated operations)
        ctx = fbuild.BuildContext(
            project_dir=Path("my_project"),
            environment="esp32dev"
        )
        fbuild.Daemon.install_dependencies(ctx)
        fbuild.Daemon.build(ctx)
        fbuild.Daemon.deploy(ctx)

        # Option 2: Use individual parameters
        fbuild.Daemon.build(
            project_dir=Path("my_project"),
            environment="esp32dev"
        )

        # Daemon lifecycle
        fbuild.Daemon.ensure_running()
        fbuild.Daemon.status()
        fbuild.Daemon.stop()
    """

    @staticmethod
    def ensure_running() -> bool:
        """Ensure the fbuild daemon is running.

        Starts the daemon if not already running.

        Returns:
            True if daemon is running or was started successfully, False otherwise.
        """
        from fbuild.daemon import ensure_daemon_running

        ensure_daemon_running()
        return True

    @staticmethod
    def stop() -> bool:
        """Stop the fbuild daemon.

        Returns:
            True if daemon was stopped, False otherwise.
        """
        from fbuild.daemon import stop_daemon

        return stop_daemon()

    @staticmethod
    def status() -> dict[str, Any]:
        """Get current fbuild daemon status.

        Returns:
            Dictionary with daemon status information including:
            - state: Current daemon state
            - message: Status message
            - running: Whether daemon is running
        """
        from fbuild.daemon import get_daemon_status

        return get_daemon_status()

    @staticmethod
    def install_dependencies(
        ctx_or_project_dir: BuildContext | Path,
        environment: str | None = None,
        verbose: bool = False,
        timeout: float = 1800,
    ) -> bool:
        """Pre-install toolchain, platform, framework, and libraries.

        This downloads and caches all dependencies required for a build
        without actually compiling. Useful for:
        - Pre-warming the cache before builds
        - Ensuring dependencies are available offline
        - Separating dependency installation from compilation

        Can be called with a BuildContext or individual parameters:

            # Using BuildContext
            ctx = fbuild.BuildContext(project_dir=Path("."), environment="esp32dev")
            fbuild.Daemon.install_dependencies(ctx)

            # Using individual parameters
            fbuild.Daemon.install_dependencies(
                project_dir=Path("."),
                environment="esp32dev"
            )

        Args:
            ctx_or_project_dir: BuildContext or Path to project directory
            environment: Build environment name (ignored if BuildContext passed)
            verbose: Enable verbose output (ignored if BuildContext passed)
            timeout: Maximum wait time in seconds (ignored if BuildContext passed)

        Returns:
            True if dependencies installed successfully, False otherwise.
        """
        from fbuild.daemon import request_install_dependencies

        # Handle BuildContext or individual parameters
        if isinstance(ctx_or_project_dir, BuildContext):
            ctx = ctx_or_project_dir
            return request_install_dependencies(
                project_dir=ctx.project_dir,
                environment=ctx.environment,
                verbose=ctx.verbose,
                timeout=ctx.timeout,
            )
        else:
            if environment is None:
                raise ValueError("environment is required when not using BuildContext")
            return request_install_dependencies(
                project_dir=ctx_or_project_dir,
                environment=environment,
                verbose=verbose,
                timeout=timeout,
            )

    @staticmethod
    def build(
        ctx_or_project_dir: BuildContext | Path,
        environment: str | None = None,
        clean_build: bool = False,
        verbose: bool = False,
        timeout: float = 1800,
    ) -> bool:
        """Request a build operation from the daemon.

        Can be called with a BuildContext or individual parameters:

            # Using BuildContext
            ctx = fbuild.BuildContext(project_dir=Path("."), environment="esp32dev")
            fbuild.Daemon.build(ctx)

            # Using individual parameters
            fbuild.Daemon.build(
                project_dir=Path("."),
                environment="esp32dev"
            )

        Args:
            ctx_or_project_dir: BuildContext or Path to project directory
            environment: Build environment name (ignored if BuildContext passed)
            clean_build: Whether to perform a clean build (ignored if BuildContext passed)
            verbose: Enable verbose build output (ignored if BuildContext passed)
            timeout: Maximum wait time in seconds (ignored if BuildContext passed)

        Returns:
            True if build successful, False otherwise.
        """
        from fbuild.daemon import request_build

        # Handle BuildContext or individual parameters
        if isinstance(ctx_or_project_dir, BuildContext):
            ctx = ctx_or_project_dir
            return request_build(
                project_dir=ctx.project_dir,
                environment=ctx.environment,
                clean_build=ctx.clean_build,
                verbose=ctx.verbose,
                timeout=ctx.timeout,
            )
        else:
            if environment is None:
                raise ValueError("environment is required when not using BuildContext")
            return request_build(
                project_dir=ctx_or_project_dir,
                environment=environment,
                clean_build=clean_build,
                verbose=verbose,
                timeout=timeout,
            )

    @staticmethod
    def deploy(
        ctx_or_project_dir: BuildContext | Path,
        environment: str | None = None,
        port: str | None = None,
        clean_build: bool = False,
        monitor_after: bool = False,
        monitor_timeout: float | None = None,
        monitor_halt_on_error: str | None = None,
        monitor_halt_on_success: str | None = None,
        monitor_expect: str | None = None,
        timeout: float = 1800,
    ) -> bool:
        """Request a deploy (build + upload) operation from the daemon.

        Can be called with a BuildContext or individual parameters:

            # Using BuildContext
            ctx = fbuild.BuildContext(
                project_dir=Path("."),
                environment="esp32dev",
                port="COM3"
            )
            fbuild.Daemon.deploy(ctx)

            # Using individual parameters
            fbuild.Daemon.deploy(
                project_dir=Path("."),
                environment="esp32dev",
                port="COM3"
            )

        Args:
            ctx_or_project_dir: BuildContext or Path to project directory
            environment: Build environment name (ignored if BuildContext passed)
            port: Serial port for upload (ignored if BuildContext passed)
            clean_build: Whether to perform a clean build (ignored if BuildContext passed)
            monitor_after: Whether to start monitor after deploy
            monitor_timeout: Timeout for monitor (if monitor_after=True)
            monitor_halt_on_error: Pattern to halt on error (if monitor_after=True)
            monitor_halt_on_success: Pattern to halt on success (if monitor_after=True)
            monitor_expect: Expected pattern to check (if monitor_after=True)
            timeout: Maximum wait time in seconds (ignored if BuildContext passed)

        Returns:
            True if deploy successful, False otherwise.
        """
        from fbuild.daemon import request_deploy

        # Handle BuildContext or individual parameters
        if isinstance(ctx_or_project_dir, BuildContext):
            ctx = ctx_or_project_dir
            return request_deploy(
                project_dir=ctx.project_dir,
                environment=ctx.environment,
                port=ctx.port,
                clean_build=ctx.clean_build,
                monitor_after=monitor_after,
                monitor_timeout=monitor_timeout,
                monitor_halt_on_error=monitor_halt_on_error,
                monitor_halt_on_success=monitor_halt_on_success,
                monitor_expect=monitor_expect,
                timeout=ctx.timeout,
            )
        else:
            if environment is None:
                raise ValueError("environment is required when not using BuildContext")
            return request_deploy(
                project_dir=ctx_or_project_dir,
                environment=environment,
                port=port,
                clean_build=clean_build,
                monitor_after=monitor_after,
                monitor_timeout=monitor_timeout,
                monitor_halt_on_error=monitor_halt_on_error,
                monitor_halt_on_success=monitor_halt_on_success,
                monitor_expect=monitor_expect,
                timeout=timeout,
            )

    @staticmethod
    def monitor(
        ctx_or_project_dir: BuildContext | Path,
        environment: str | None = None,
        port: str | None = None,
        baud_rate: int | None = None,
        halt_on_error: str | None = None,
        halt_on_success: str | None = None,
        expect: str | None = None,
        timeout: float | None = None,
    ) -> bool:
        """Request a monitor operation from the daemon.

        Can be called with a BuildContext or individual parameters.

        Args:
            ctx_or_project_dir: BuildContext or Path to project directory
            environment: Build environment name (ignored if BuildContext passed)
            port: Serial port (ignored if BuildContext passed)
            baud_rate: Serial baud rate (optional)
            halt_on_error: Pattern to halt on (error detection)
            halt_on_success: Pattern to halt on (success detection)
            expect: Expected pattern to check at timeout/success
            timeout: Maximum monitoring time in seconds

        Returns:
            True if monitoring successful, False otherwise.
        """
        from fbuild.daemon import request_monitor

        # Handle BuildContext or individual parameters
        if isinstance(ctx_or_project_dir, BuildContext):
            ctx = ctx_or_project_dir
            return request_monitor(
                project_dir=ctx.project_dir,
                environment=ctx.environment,
                port=ctx.port,
                baud_rate=baud_rate,
                halt_on_error=halt_on_error,
                halt_on_success=halt_on_success,
                expect=expect,
                timeout=timeout if timeout is not None else ctx.timeout,
            )
        else:
            if environment is None:
                raise ValueError("environment is required when not using BuildContext")
            return request_monitor(
                project_dir=ctx_or_project_dir,
                environment=environment,
                port=port,
                baud_rate=baud_rate,
                halt_on_error=halt_on_error,
                halt_on_success=halt_on_success,
                expect=expect,
                timeout=timeout,
            )


__all__ = [
    "__version__",
    "is_available",
    "BuildContext",
    "Daemon",
    "DaemonConnection",
    "connect_daemon",
]
