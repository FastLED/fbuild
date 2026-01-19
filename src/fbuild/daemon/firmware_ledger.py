"""
Firmware Ledger - Track deployed firmware on devices.

This module provides a ledger to track what firmware is currently deployed on each
device/port, allowing clients to skip re-upload if the same firmware is already running.
The cache is stored in ~/.fbuild/firmware_ledger.json (or dev path if FBUILD_DEV_MODE)
and uses file locking for thread-safe access.

Features:
- Port to firmware hash mapping with timestamps
- Source file hash tracking for change detection
- Build flags hash for build configuration tracking
- Automatic stale entry expiration (configurable, default 24 hours)
- Thread-safe file access with file locking
- Skip re-upload when firmware matches what's deployed

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
import sys
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
        # Use project-local directory for development
        return Path.cwd() / ".fbuild" / "daemon_dev" / "firmware_ledger.json"
    else:
        # Use home directory for production
        return Path.home() / ".fbuild" / "firmware_ledger.json"


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


class FirmwareLedger:
    """Manages port to firmware mapping with persistent storage.

    The ledger stores mappings in ~/.fbuild/firmware_ledger.json (or dev path)
    and provides thread-safe access through file locking.

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

    def _read_ledger(self) -> dict[str, dict[str, Any]]:
        """Read the ledger file.

        Returns:
            Dictionary mapping port names to entry dictionaries
        """
        if not self._ledger_path.exists():
            return {}

        try:
            with open(self._ledger_path, encoding="utf-8") as f:
                data = json.load(f)
                if not isinstance(data, dict):
                    return {}
                return data
        except (json.JSONDecodeError, OSError):
            return {}

    def _write_ledger(self, data: dict[str, dict[str, Any]]) -> None:
        """Write the ledger file.

        Args:
            data: Dictionary mapping port names to entry dictionaries
        """
        self._ensure_directory()
        try:
            with open(self._ledger_path, "w", encoding="utf-8") as f:
                json.dump(data, f, indent=2)
        except OSError as e:
            raise FirmwareLedgerError(f"Failed to write ledger: {e}") from e

    def _acquire_file_lock(self) -> Any:
        """Acquire a file lock for cross-process synchronization.

        Returns:
            Lock file handle (or None on platforms without locking support)
        """
        self._ensure_directory()
        lock_path = self._ledger_path.with_suffix(".lock")

        try:
            # Open lock file
            lock_file = open(lock_path, "w", encoding="utf-8")

            # Platform-specific locking
            if sys.platform == "win32":
                import msvcrt

                msvcrt.locking(lock_file.fileno(), msvcrt.LK_NBLCK, 1)
            else:  # pragma: no cover - Unix only
                import fcntl  # type: ignore[import-not-found]

                fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX)

            return lock_file
        except (ImportError, OSError):
            # Locking not available or failed - continue without lock
            return None

    def _release_file_lock(self, lock_file: Any) -> None:
        """Release a file lock.

        Args:
            lock_file: Lock file handle from _acquire_file_lock
        """
        if lock_file is None:
            return

        try:
            if sys.platform == "win32":
                import msvcrt

                try:
                    msvcrt.locking(lock_file.fileno(), msvcrt.LK_UNLCK, 1)
                except OSError:
                    pass
            else:  # pragma: no cover - Unix only
                import fcntl  # type: ignore[import-not-found]

                fcntl.flock(lock_file.fileno(), fcntl.LOCK_UN)

            lock_file.close()
        except (ImportError, OSError):
            pass

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
            lock_file = self._acquire_file_lock()
            try:
                data = self._read_ledger()
                data[port] = entry.to_dict()
                self._write_ledger(data)
            finally:
                self._release_file_lock(lock_file)

    def get_deployment(self, port: str) -> FirmwareEntry | None:
        """Get the deployment entry for a port.

        Args:
            port: Serial port name (e.g., "COM3", "/dev/ttyUSB0")

        Returns:
            FirmwareEntry or None if not found or stale
        """
        with self._lock:
            lock_file = self._acquire_file_lock()
            try:
                data = self._read_ledger()
                entry_data = data.get(port)
                if entry_data is None:
                    return None

                entry = FirmwareEntry.from_dict(entry_data)
                if entry.is_stale():
                    return None

                return entry
            finally:
                self._release_file_lock(lock_file)

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
            lock_file = self._acquire_file_lock()
            try:
                data = self._read_ledger()
                if port in data:
                    del data[port]
                    self._write_ledger(data)
                    return True
                return False
            finally:
                self._release_file_lock(lock_file)

    def clear_all(self) -> int:
        """Clear all entries from the ledger.

        Returns:
            Number of entries cleared
        """
        with self._lock:
            lock_file = self._acquire_file_lock()
            try:
                data = self._read_ledger()
                count = len(data)
                self._write_ledger({})
                return count
            finally:
                self._release_file_lock(lock_file)

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
            lock_file = self._acquire_file_lock()
            try:
                data = self._read_ledger()
                original_count = len(data)

                # Filter out stale entries
                fresh_data = {}
                for port, entry_data in data.items():
                    entry = FirmwareEntry.from_dict(entry_data)
                    if not entry.is_stale(threshold_seconds):
                        fresh_data[port] = entry_data

                self._write_ledger(fresh_data)
                return original_count - len(fresh_data)
            finally:
                self._release_file_lock(lock_file)

    def get_all(self) -> dict[str, FirmwareEntry]:
        """Get all non-stale entries in the ledger.

        Returns:
            Dictionary mapping port names to FirmwareEntry objects
        """
        with self._lock:
            lock_file = self._acquire_file_lock()
            try:
                data = self._read_ledger()
                result = {}
                for port, entry_data in data.items():
                    entry = FirmwareEntry.from_dict(entry_data)
                    if not entry.is_stale():
                        result[port] = entry
                return result
            finally:
                self._release_file_lock(lock_file)


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
