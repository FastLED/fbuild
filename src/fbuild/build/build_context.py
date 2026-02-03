"""Build Context - Aggregated build configuration.

This module defines:
- BuildParams: Basic build parameters from CLI/daemon (used by build_processor)
- BuildContext: Full build context with platform info (created by orchestrators)

Design:
    BuildParams flows from CLI -> daemon -> orchestrator with basic params.
    BuildContext is created by the orchestrator after platform initialization,
    containing pre-resolved profile flags, platform, toolchain, and compilation
    infrastructure. BuildContext flows through compiler and linker.
"""

from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Any, Dict, List, Optional

from .build_profiles import BuildProfile, ProfileFlags, get_profile

if TYPE_CHECKING:
    from fbuild.daemon.compilation_queue import CompilationJobQueue
    from fbuild.packages.package import IPackage, IToolchain, IFramework
    from fbuild.build.compilation_executor import CompilationExecutor


@dataclass(frozen=True)
class BuildParams:
    """Basic build parameters from CLI/daemon.

    This dataclass contains the minimal parameters needed to start a build,
    passed from build_processor to orchestrator. The orchestrator uses these
    to initialize the platform and create the full BuildContext.

    Attributes:
        project_dir: Project root directory containing platformio.ini
        env_name: Environment name to build
        clean: Whether to clean build artifacts before building
        profile: Build profile enum value
        profile_flags: Pre-resolved profile flags
        queue: Compilation queue for parallel compilation
        build_dir: Build directory path incorporating profile name
        verbose: Whether to enable verbose output
    """

    project_dir: Path
    env_name: str
    clean: bool
    profile: BuildProfile
    profile_flags: ProfileFlags
    queue: "CompilationJobQueue"
    build_dir: Path
    verbose: bool

    @classmethod
    def create(
        cls,
        project_dir: Path,
        env_name: str,
        clean: bool,
        profile: BuildProfile,
        queue: "CompilationJobQueue",
        build_dir: Path,
        verbose: bool,
    ) -> "BuildParams":
        """Create a BuildParams with resolved profile flags."""
        return cls(
            project_dir=project_dir,
            env_name=env_name,
            clean=clean,
            profile=profile,
            profile_flags=get_profile(profile),
            queue=queue,
            build_dir=build_dir,
            verbose=verbose,
        )


@dataclass(frozen=True)
class BuildContext:
    """Full build context with platform info, created by orchestrators.

    This dataclass contains ALL build configuration needed by compiler and linker.
    It's created by the orchestrator after platform initialization, combining
    the basic BuildParams with platform-specific information.

    Attributes:
        project_dir: Project root directory containing platformio.ini
        env_name: Environment name to build
        clean: Whether to clean build artifacts before building
        profile: Build profile enum value (for debugging/logging)
        profile_flags: Pre-resolved profile flags (compile_flags, link_flags, etc.)
        queue: Compilation queue for parallel compilation
        build_dir: Build directory path incorporating profile name (e.g., .fbuild/uno/release)
        verbose: Whether to enable verbose output
        platform: Platform package instance
        toolchain: Toolchain instance for compiler/linker paths
        mcu: MCU type string (e.g., "esp32c6", "atmega328p")
        framework_version: Framework version string (e.g., "3.0.7")
        cache: Optional cache for trampoline support on Windows
        compilation_executor: Executor for running compilation jobs

        # New fields for consolidated build configuration (Phase 1 of BuildContext consolidation)
        framework: Framework instance (for core sources, includes, etc.)
        board_id: Board identifier (e.g., "esp32-c6-devkitm-1", "teensy41")
        board_config: Board configuration dictionary (loaded once from platform)
        platform_config: Platform configuration dictionary (loaded once from platform_configs)
        variant: Board variant name (e.g., "esp32c6", "teensy41")
        core: Arduino core name (defaults to "arduino")
        user_build_flags: Build flags from platformio.ini
        env_config: Environment configuration from platformio.ini
    """

    # Core build parameters
    project_dir: Path
    env_name: str
    clean: bool
    profile: BuildProfile
    profile_flags: ProfileFlags
    queue: "CompilationJobQueue"
    build_dir: Path
    verbose: bool

    # Platform and toolchain
    platform: "IPackage"
    toolchain: "IToolchain"
    mcu: str
    framework_version: Optional[str]
    cache: Optional[Any]
    compilation_executor: "CompilationExecutor"

    # Consolidated build configuration (all mandatory)
    framework: "IFramework"
    board_id: str
    board_config: Dict[str, Any]
    platform_config: Dict[str, Any]
    variant: str
    core: str
    user_build_flags: List[str]
    env_config: Dict[str, Any]

    @classmethod
    def from_request(
        cls,
        request: "BuildParams",
        platform: "IPackage",
        toolchain: "IToolchain",
        mcu: str,
        framework_version: Optional[str],
        compilation_executor: "CompilationExecutor",
        cache: Optional[Any],
        # Consolidated build configuration
        framework: "IFramework",
        board_id: str,
        board_config: Dict[str, Any],
        platform_config: Dict[str, Any],
        variant: str,
        core: str,
        user_build_flags: List[str],
        env_config: Dict[str, Any],
    ) -> "BuildContext":
        """Create a BuildContext from a BuildParams plus platform-specific info.

        This is the primary factory method. Orchestrators call this after
        initializing the platform, toolchain, and loading configuration once.

        Args:
            request: Basic build request from build_processor
            platform: Platform package instance
            toolchain: Toolchain instance
            mcu: MCU type string
            framework_version: Framework version string
            compilation_executor: Executor for running compilation jobs
            cache: Cache for trampoline support
            framework: Framework instance (for core sources, includes, etc.)
            board_id: Board identifier (e.g., "esp32-c6-devkitm-1")
            board_config: Board configuration dictionary (loaded once)
            platform_config: Platform configuration dictionary (loaded once)
            variant: Board variant name
            core: Arduino core name
            user_build_flags: Build flags from platformio.ini
            env_config: Environment configuration from platformio.ini

        Returns:
            Full BuildContext ready for compiler/linker
        """
        return cls(
            project_dir=request.project_dir,
            env_name=request.env_name,
            clean=request.clean,
            profile=request.profile,
            profile_flags=request.profile_flags,
            queue=request.queue,
            build_dir=request.build_dir,
            verbose=request.verbose,
            platform=platform,
            toolchain=toolchain,
            mcu=mcu,
            framework_version=framework_version,
            cache=cache,
            compilation_executor=compilation_executor,
            framework=framework,
            board_id=board_id,
            board_config=board_config,
            platform_config=platform_config,
            variant=variant,
            core=core,
            user_build_flags=user_build_flags,
            env_config=env_config,
        )

    @property
    def compile_flags(self) -> tuple[str, ...]:
        """Get compilation flags from the resolved profile."""
        return self.profile_flags.compile_flags

    @property
    def link_flags(self) -> tuple[str, ...]:
        """Get linker flags from the resolved profile."""
        return self.profile_flags.link_flags

    @property
    def profile_name(self) -> str:
        """Get the profile name string (e.g., 'release', 'quick')."""
        return self.profile.value
