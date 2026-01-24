"""Abstract base class for firmware deployers.

This module defines the interface for platform-specific deployers
to ensure consistent behavior across different platforms.
"""

from abc import ABC, abstractmethod
from dataclasses import dataclass
from pathlib import Path
from typing import Optional


@dataclass
class DeploymentResult:
    """Result of a firmware deployment operation."""

    success: bool
    message: str
    port: Optional[str] = None


class DeploymentError(Exception):
    """Base exception for deployment errors."""

    pass


class IDeployer(ABC):
    """Interface for firmware deployers.

    Deployers handle uploading firmware to embedded devices:
    1. Locate firmware binaries
    2. Detect or validate serial port (using shared serial_utils)
    3. Flash firmware to device
    4. Verify upload success

    Note: Serial port detection is now provided by the shared
    serial_utils.detect_serial_port() function instead of requiring
    each deployer to implement its own detection logic.
    """

    @abstractmethod
    def deploy(
        self,
        project_dir: Path,
        env_name: str,
        port: Optional[str] = None,
    ) -> DeploymentResult:
        """Deploy firmware to a device.

        Args:
            project_dir: Path to project directory
            env_name: Environment name to deploy
            port: Serial port to use (auto-detect if None using serial_utils.detect_serial_port())

        Returns:
            DeploymentResult with success status and message

        Raises:
            DeploymentError: If deployment fails
        """
        pass
