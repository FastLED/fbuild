"""
Firmware Ledger - Track deployed firmware on devices.

This module provides a ledger to track what firmware is currently deployed on each
device/port, allowing clients to skip re-upload if the same firmware is already running.
The cache is stored in ~/.fbuild/firmware_ledger.json (or dev path if FBUILD_DEV_MODE).

Features:
- Port to firmware hash mapping with timestamps
- Source file hash tracking for change detection
- Build flags hash for build configuration tracking
- Automatic stale entry expiration (configurable, default 24 hours)
- Thread-safe in-process access via threading.Lock
- Skip re-upload when firmware matches what's deployed

Note: Cross-process synchronization is handled by the daemon which holds locks in memory.

Example:
    >>> from fbuild.daemon.firmware_ledger import FirmwareLedger, compute_firmware_hash
    >>>
    >>> # Record a deployment
    >>> ledger = FirmwareLedger()
    >>> fw_hash = compute_firmware_hash(Path("firmware.bin"))
    >>> ledger.record_deployment("COM3", fw_hash, "abc123", "/path/to/project", "esp32dev")
    >>>
    >>> # Check if firmware is current
    >>> if ledger.is_current("COM3", fw_hash, "abc123"):
    >>>     print("Firmware already deployed, skipping upload")
"""

import hashlib
import json
import os
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

# Stale entry threshold: 24 hours (in seconds)
DEFAULT_STALE_THRESHOLD_SECONDS = 24 * 60 * 60


def _get_ledger_path() -> Path:
    """Get the path to the firmware ledger file.

    Returns:
        Path to firmware_ledger.json, respecting FBUILD_DEV_MODE
    """
    if os.environ.get("FBUILD_DEV_MODE") == "1":
        # Use global ~/.fbuild/daemon_dev/ for development
        return Path.home() / ".fbuild" / "daemon_dev" / "firmware_ledger.json"
    else:
        # Use global ~/.fbuild/daemon/ for production
        return Path.home() / ".fbuild" / "daemon" / "firmware_ledger.json"


class FirmwareLedgerError(Exception):
    """Raised when firmware ledger operations fail."""

    pass


@dataclass
class FirmwareEntry:
    """A single entry in the firmware ledger.

    Attributes:
        port: Serial port name (e.g., "COM3", "/dev/ttyUSB0")
        firmware_hash: SHA256 hash of the firmware file (.bin/.hex)
        source_hash: Combined hash of all source files
        project_dir: Absolute path to the project directory
        environment: Build environment name (e.g., "esp32dev", "uno")
        upload_timestamp: Unix timestamp when firmware was uploaded
        build_flags_hash: Optional hash of build flags (for detecting config changes)
    """

    port: str
    firmware_hash: str
    source_hash: str
    project_dir: str
    environment: str
    upload_timestamp: float
    build_flags_hash: str | None = None

    def is_stale(self, threshold: float = DEFAULT_STALE_THRESHOLD_SECONDS) -> bool:
        """Check if this entry is stale (older than threshold).

        Args:
            threshold: Maximum age in seconds before entry is considered stale

        Returns:
            True if entry is older than threshold
        """
        return (time.time() - self.upload_timestamp) > threshold

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return {
            "port": self.port,
            "firmware_hash": self.firmware_hash,
            "source_hash": self.source_hash,
            "project_dir": self.project_dir,
            "environment": self.environment,
            "upload_timestamp": self.upload_timestamp,
            "build_flags_hash": self.build_flags_hash,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "FirmwareEntry":
        """Create entry from dictionary.

        Args:
            data: Dictionary with entry fields

        Returns:
            FirmwareEntry instance
        """
        return cls(
            port=data["port"],
            firmware_hash=data["firmware_hash"],
            source_hash=data["source_hash"],
            project_dir=data["project_dir"],
            environment=data["environment"],
            upload_timestamp=data["upload_timestamp"],
            build_flags_hash=data.get("build_flags_hash"),
        )


@dataclass
class FirmwareLedgerData:
    """Container for firmware ledger entries with type safety.

    Attributes:
        entries: Dictionary mapping port names to FirmwareEntry objects
    """

    entries: dict[str, FirmwareEntry]

    def to_dict(self) -> dict[str, dict[str, Any]]:
        """Convert to dictionary for JSON serialization.

        Returns:
            Dictionary mapping port names to entry dictionaries
        """
        return {port: entry.to_dict() for port, entry in self.entries.items()}

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "FirmwareLedgerData":
        """Create FirmwareLedgerData from dictionary.

        Args:
            data: Dictionary mapping port names to entry dictionaries

        Returns:
            FirmwareLedgerData instance with parsed entries
        """
        entries = {}
        for port, entry_data in data.items():
            if isinstance(entry_data, dict):
                entries[port] = FirmwareEntry.from_dict(entry_data)
        return cls(entries=entries)


class FirmwareLedger:
    """Manages port to firmware mapping with persistent storage.

    The ledger stores mappings in ~/.fbuild/firmware_ledger.json (or dev path)
    and provides thread-safe in-process access through threading.Lock.
    Cross-process synchronization is handled by the daemon which holds locks in memory.

    Example:
        >>> ledger = FirmwareLedger()
        >>> ledger.record_deployment("COM3", "abc123", "def456", "/path/project", "esp32dev")
        >>> entry = ledger.get_deployment("COM3")
        >>> print(entry.firmware_hash if entry else "Not found")
        >>> ledger.clear("COM3")
    """

    def __init__(self, ledger_path: Path | None = None):
        """Initialize the firmware ledger.

        Args:
            ledger_path: Optional custom path for ledger file.
                        Defaults to ~/.fbuild/firmware_ledger.json (or dev path)
        """
        if ledger_path is None:
            self._ledger_path = _get_ledger_path()
        else:
            self._ledger_path = ledger_path

        # Thread lock for in-process synchronization
        self._lock = threading.Lock()

    @property
    def ledger_path(self) -> Path:
        """Get the path to the ledger file."""
        return self._ledger_path

    def _ensure_directory(self) -> None:
        """Ensure the parent directory exists."""
        self._ledger_path.parent.mkdir(parents=True, exist_ok=True)

    def _read_ledger(self) -> FirmwareLedgerData:
        """Read the ledger file.

        Returns:
            FirmwareLedgerData instance with all ledger entries
        """
        if not self._ledger_path.exists():
            return FirmwareLedgerData(entries={})

        try:
            with open(self._ledger_path, encoding="utf-8") as f:
                data = json.load(f)
                if not isinstance(data, dict):
                    return FirmwareLedgerData(entries={})
                return FirmwareLedgerData.from_dict(data)
        except (json.JSONDecodeError, OSError):
            return FirmwareLedgerData(entries={})

    def _write_ledger(self, data: FirmwareLedgerData) -> None:
        """Write the ledger file.

        Args:
            data: FirmwareLedgerData instance with all ledger entries
        """
        self._ensure_directory()
        try:
            with open(self._ledger_path, "w", encoding="utf-8") as f:
                json.dump(data.to_dict(), f, indent=2)
        except OSError as e:
            raise FirmwareLedgerError(f"Failed to write ledger: {e}") from e

    def record_deployment(
        self,
        port: str,
        firmware_hash: str,
        source_hash: str,
        project_dir: str,
        environment: str,
        build_flags_hash: str | None = None,
    ) -> None:
        """Record that firmware was deployed to a port.

        Args:
            port: Serial port name (e.g., "COM3", "/dev/ttyUSB0")
            firmware_hash: SHA256 hash of the firmware file
            source_hash: Combined hash of all source files
            project_dir: Absolute path to the project directory
            environment: Build environment name (e.g., "esp32dev")
            build_flags_hash: Optional hash of build flags
        """
        entry = FirmwareEntry(
            port=port,
            firmware_hash=firmware_hash,
            source_hash=source_hash,
            project_dir=str(project_dir),
            environment=environment,
            upload_timestamp=time.time(),
            build_flags_hash=build_flags_hash,
        )

        with self._lock:
            ledger_data = self._read_ledger()
            ledger_data.entries[port] = entry
            self._write_ledger(ledger_data)

    def get_deployment(self, port: str) -> FirmwareEntry | None:
        """Get the deployment entry for a port.

        Args:
            port: Serial port name (e.g., "COM3", "/dev/ttyUSB0")

        Returns:
            FirmwareEntry or None if not found or stale
        """
        with self._lock:
            ledger_data = self._read_ledger()
            entry = ledger_data.entries.get(port)
            if entry is None:
                return None

            if entry.is_stale():
                return None

            return entry

    def is_current(
        self,
        port: str,
        firmware_hash: str,
        source_hash: str,
    ) -> bool:
        """Check if firmware matches what's currently deployed.

        This is used to determine if we can skip re-uploading firmware.

        Args:
            port: Serial port name
            firmware_hash: SHA256 hash of the firmware file
            source_hash: Combined hash of source files

        Returns:
            True if the firmware and source hashes match the deployed version
        """
        entry = self.get_deployment(port)
        if entry is None:
            return False

        return entry.firmware_hash == firmware_hash and entry.source_hash == source_hash

    def needs_redeploy(
        self,
        port: str,
        source_hash: str,
        build_flags_hash: str | None = None,
    ) -> bool:
        """Check if source has changed and needs redeployment.

        This checks if the source files or build configuration have changed
        since the last deployment.

        Args:
            port: Serial port name
            source_hash: Current combined hash of source files
            build_flags_hash: Current hash of build flags (optional)

        Returns:
            True if source or build flags have changed (needs redeploy),
            False if same source and flags (can skip build/deploy)
        """
        entry = self.get_deployment(port)
        if entry is None:
            # No previous deployment, needs deploy
            return True

        # Check source hash
        if entry.source_hash != source_hash:
            return True

        # Check build flags if provided
        if build_flags_hash is not None and entry.build_flags_hash != build_flags_hash:
            return True

        return False

    def clear(self, port: str) -> bool:
        """Clear the entry for a port.

        Use this when a device is reset or when you want to force a re-upload.

        Args:
            port: Serial port name to clear

        Returns:
            True if entry was cleared, False if not found
        """
        with self._lock:
            ledger_data = self._read_ledger()
            if port in ledger_data.entries:
                del ledger_data.entries[port]
                self._write_ledger(ledger_data)
                return True
            return False

    def clear_all(self) -> int:
        """Clear all entries from the ledger.

        Returns:
            Number of entries cleared
        """
        with self._lock:
            ledger_data = self._read_ledger()
            count = len(ledger_data.entries)
            self._write_ledger(FirmwareLedgerData(entries={}))
            return count

    def clear_stale(
        self,
        threshold_seconds: float = DEFAULT_STALE_THRESHOLD_SECONDS,
    ) -> int:
        """Remove all stale entries from the ledger.

        Args:
            threshold_seconds: Maximum age in seconds before entry is considered stale

        Returns:
            Number of entries removed
        """
        with self._lock:
            ledger_data = self._read_ledger()
            original_count = len(ledger_data.entries)

            # Filter out stale entries
            fresh_entries = {}
            for port, entry in ledger_data.entries.items():
                if not entry.is_stale(threshold_seconds):
                    fresh_entries[port] = entry

            self._write_ledger(FirmwareLedgerData(entries=fresh_entries))
            return original_count - len(fresh_entries)

    def get_all(self) -> dict[str, FirmwareEntry]:
        """Get all non-stale entries in the ledger.

        Returns:
            Dictionary mapping port names to FirmwareEntry objects
        """
        with self._lock:
            ledger_data = self._read_ledger()
            result = {}
            for port, entry in ledger_data.entries.items():
                if not entry.is_stale():
                    result[port] = entry
            return result


def compute_firmware_hash(firmware_path: Path) -> str:
    """Compute SHA256 hash of a firmware file.

    Args:
        firmware_path: Path to firmware file (.bin, .hex, etc.)

    Returns:
        Hexadecimal SHA256 hash string

    Raises:
        FirmwareLedgerError: If file cannot be read
    """
    try:
        hasher = hashlib.sha256()
        with open(firmware_path, "rb") as f:
            # Read in chunks for large files
            for chunk in iter(lambda: f.read(65536), b""):
                hasher.update(chunk)
        return hasher.hexdigest()
    except OSError as e:
        raise FirmwareLedgerError(f"Failed to hash firmware file: {e}") from e


def compute_source_hash(source_files: list[Path]) -> str:
    """Compute combined hash of multiple source files.

    The hash is computed by hashing each file's content in sorted order
    (by path) to ensure deterministic results.

    Args:
        source_files: List of source file paths

    Returns:
        Hexadecimal SHA256 hash string representing all source files

    Raises:
        FirmwareLedgerError: If any file cannot be read
    """
    hasher = hashlib.sha256()

    # Sort files by path for deterministic ordering
    sorted_files = sorted(source_files, key=lambda p: str(p))

    for file_path in sorted_files:
        try:
            # Include the relative path in the hash for detecting file renames/moves
            hasher.update(str(file_path).encode("utf-8"))
            hasher.update(b"\x00")  # Null separator

            with open(file_path, "rb") as f:
                for chunk in iter(lambda: f.read(65536), b""):
                    hasher.update(chunk)
            hasher.update(b"\x00")  # Separator between files
        except OSError as e:
            raise FirmwareLedgerError(f"Failed to hash source file {file_path}: {e}") from e

    return hasher.hexdigest()


def compute_build_flags_hash(build_flags: list[str] | str | None) -> str:
    """Compute hash of build flags.

    Args:
        build_flags: Build flags as a list of strings or a single string

    Returns:
        Hexadecimal SHA256 hash string
    """
    hasher = hashlib.sha256()

    if build_flags is None:
        return hasher.hexdigest()

    if isinstance(build_flags, str):
        build_flags = [build_flags]

    # Sort flags for deterministic ordering
    sorted_flags = sorted(build_flags)
    for flag in sorted_flags:
        hasher.update(flag.encode("utf-8"))
        hasher.update(b"\x00")

    return hasher.hexdigest()
