"""
Firmware ledger messages for tracking deployed firmware.

This module defines messages for querying and recording firmware deployments.
"""

import time
from dataclasses import asdict, dataclass, field
from typing import Any


@dataclass
class FirmwareQueryRequest:
    """Client → Daemon: Query if firmware is current on a device.

    Used to check if a redeploy is needed or if the device already has
    the expected firmware loaded.

    Attributes:
        port: Serial port of the device
        source_hash: Hash of the source files
        build_flags_hash: Hash of the build flags (optional)
        timestamp: Unix timestamp when request was created
    """

    port: str
    source_hash: str
    build_flags_hash: str | None = None
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "FirmwareQueryRequest":
        """Create FirmwareQueryRequest from dictionary."""
        return cls(
            port=data["port"],
            source_hash=data["source_hash"],
            build_flags_hash=data.get("build_flags_hash"),
            timestamp=data.get("timestamp", time.time()),
        )


@dataclass
class FirmwareQueryResponse:
    """Daemon → Client: Response to firmware query.

    Attributes:
        is_current: True if firmware matches what's deployed (no redeploy needed)
        needs_redeploy: True if source or build flags have changed
        firmware_hash: Hash of the currently deployed firmware (if known)
        project_dir: Project directory of the deployed firmware
        environment: Environment of the deployed firmware
        upload_timestamp: When the firmware was last uploaded
        message: Human-readable message
        timestamp: Unix timestamp of the response
    """

    is_current: bool
    needs_redeploy: bool
    firmware_hash: str | None = None
    project_dir: str | None = None
    environment: str | None = None
    upload_timestamp: float | None = None
    message: str = ""
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "FirmwareQueryResponse":
        """Create FirmwareQueryResponse from dictionary."""
        return cls(
            is_current=data["is_current"],
            needs_redeploy=data["needs_redeploy"],
            firmware_hash=data.get("firmware_hash"),
            project_dir=data.get("project_dir"),
            environment=data.get("environment"),
            upload_timestamp=data.get("upload_timestamp"),
            message=data.get("message", ""),
            timestamp=data.get("timestamp", time.time()),
        )


@dataclass
class FirmwareRecordRequest:
    """Client → Daemon: Record a firmware deployment.

    Sent after a successful upload to update the firmware ledger.

    Attributes:
        port: Serial port of the device
        firmware_hash: Hash of the firmware file
        source_hash: Hash of the source files
        project_dir: Absolute path to project directory
        environment: Build environment name
        build_flags_hash: Hash of build flags (optional)
        timestamp: Unix timestamp when request was created
    """

    port: str
    firmware_hash: str
    source_hash: str
    project_dir: str
    environment: str
    build_flags_hash: str | None = None
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "FirmwareRecordRequest":
        """Create FirmwareRecordRequest from dictionary."""
        return cls(
            port=data["port"],
            firmware_hash=data["firmware_hash"],
            source_hash=data["source_hash"],
            project_dir=data["project_dir"],
            environment=data["environment"],
            build_flags_hash=data.get("build_flags_hash"),
            timestamp=data.get("timestamp", time.time()),
        )
