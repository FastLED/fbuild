"""
Base enumerations and types for fbuild daemon messages.

This module defines core enums used across all message types.
"""

from enum import Enum


class DaemonState(Enum):
    """Daemon state enumeration."""

    IDLE = "idle"
    DEPLOYING = "deploying"
    MONITORING = "monitoring"
    BUILDING = "building"
    COMPLETED = "completed"
    FAILED = "failed"
    CANCELLED = "cancelled"  # Operation cancelled by client disconnect
    UNKNOWN = "unknown"

    @classmethod
    def from_string(cls, value: str) -> "DaemonState":
        """Convert string to DaemonState, defaulting to UNKNOWN if invalid."""
        try:
            return cls(value)
        except ValueError:
            return cls.UNKNOWN


class OperationType(Enum):
    """Type of operation being performed."""

    BUILD = "build"
    DEPLOY = "deploy"
    MONITOR = "monitor"
    BUILD_AND_DEPLOY = "build_and_deploy"
    INSTALL_DEPENDENCIES = "install_dependencies"

    @classmethod
    def from_string(cls, value: str) -> "OperationType":
        """Convert string to OperationType."""
        return cls(value)
