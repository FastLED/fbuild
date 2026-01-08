"""Abstract base class for build orchestrators.

This module defines the interface for platform-specific build orchestrators
to ensure consistent behavior across different platforms.
"""

from abc import ABC, abstractmethod
from pathlib import Path
from typing import TYPE_CHECKING, Optional
from dataclasses import dataclass

if TYPE_CHECKING:
    from zapio.build.linker import SizeInfo


@dataclass
class BuildResult:
    """Result of a complete build operation."""

    success: bool
    hex_path: Optional[Path]  # For AVR: .hex, For ESP32: .bin
    elf_path: Optional[Path]
    size_info: Optional["SizeInfo"]
    build_time: float
    message: str


class BuildOrchestratorError(Exception):
    """Base exception for build orchestration errors."""
    pass


class IBuildOrchestrator(ABC):
    """Interface for build orchestrators.

    Build orchestrators coordinate the entire build process:
    1. Parse configuration
    2. Ensure toolchain/framework packages
    3. Scan source files
    4. Compile sources
    5. Link firmware
    6. Generate binaries
    """

    @abstractmethod
    def build(
        self,
        project_dir: Path,
        env_name: Optional[str] = None,
        clean: bool = False,
        verbose: Optional[bool] = None
    ) -> BuildResult:
        """Execute complete build process.

        Args:
            project_dir: Project root directory containing platformio.ini
            env_name: Environment name to build (defaults to first/default env)
            clean: Clean build (remove all artifacts before building)
            verbose: Override verbose setting

        Returns:
            BuildResult with build status and output paths

        Raises:
            BuildOrchestratorError: If build fails at any phase
        """
        pass
