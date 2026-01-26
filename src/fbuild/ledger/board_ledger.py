"""
Board Ledger - Track attached chip/port mappings.

This module provides a simple ledger to cache chip type detections for serial ports.
The cache is stored in ~/.fbuild/board_ledger.json.

Features:
- Port to chip type mapping with timestamps
- Automatic stale entry expiration (24 hours)
- Thread-safe in-process access via threading.Lock
- Chip type validation against known ESP32 variants
- Integration with esptool for chip detection

Note: Cross-process synchronization is handled by the daemon which holds locks in memory.
"""

import json
import os
import subprocess
import sys
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from ..subprocess_utils import safe_run

# Stale entry threshold: 24 hours
STALE_THRESHOLD_SECONDS = 24 * 60 * 60

# Known chip types and their corresponding environment names
CHIP_TO_ENVIRONMENT: dict[str, str] = {
    "ESP32": "esp32dev",
    "ESP32-S2": "esp32s2",
    "ESP32-S3": "esp32s3",
    "ESP32-C2": "esp32c2",
    "ESP32-C3": "esp32c3",
    "ESP32-C6": "esp32c6",
    "ESP32-H2": "esp32h2",
}

# Valid chip types (for validation)
VALID_CHIP_TYPES = set(CHIP_TO_ENVIRONMENT.keys())


class BoardLedgerError(Exception):
    """Raised when board ledger operations fail."""

    pass


class ChipDetectionError(BoardLedgerError):
    """Raised when chip detection fails."""

    pass


@dataclass
class LedgerEntry:
    """A single entry in the board ledger.

    Attributes:
        chip_type: The detected chip type (e.g., "ESP32-S3")
        timestamp: Unix timestamp when the entry was created/updated
    """

    chip_type: str
    timestamp: float

    def is_stale(self, threshold: float = STALE_THRESHOLD_SECONDS) -> bool:
        """Check if this entry is stale (older than threshold).

        Args:
            threshold: Maximum age in seconds before entry is considered stale

        Returns:
            True if entry is older than threshold
        """
        return (time.time() - self.timestamp) > threshold

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return {
            "chip_type": self.chip_type,
            "timestamp": self.timestamp,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "LedgerEntry":
        """Create entry from dictionary."""
        return cls(
            chip_type=data["chip_type"],
            timestamp=data["timestamp"],
        )


@dataclass
class LedgerData:
    """Container for board ledger entries with type safety.

    Attributes:
        entries: Dictionary mapping port names to LedgerEntry objects
    """

    entries: dict[str, LedgerEntry]

    def to_dict(self) -> dict[str, dict[str, Any]]:
        """Convert to dictionary for JSON serialization.

        Returns:
            Dictionary mapping port names to entry dictionaries
        """
        return {port: entry.to_dict() for port, entry in self.entries.items()}

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "LedgerData":
        """Create LedgerData from dictionary.

        Args:
            data: Dictionary mapping port names to entry dictionaries

        Returns:
            LedgerData instance with parsed entries
        """
        entries = {}
        for port, entry_data in data.items():
            if isinstance(entry_data, dict):
                entries[port] = LedgerEntry.from_dict(entry_data)
        return cls(entries=entries)


class BoardLedger:
    """Manages port to chip type mappings with persistent storage.

    The ledger stores mappings in ~/.fbuild/board_ledger.json and provides
    thread-safe in-process access through threading.Lock. Cross-process
    synchronization is handled by the daemon which holds locks in memory.

    Example:
        >>> ledger = BoardLedger()
        >>> ledger.set_chip("COM3", "ESP32-S3")
        >>> chip = ledger.get_chip("COM3")
        >>> print(chip)  # "ESP32-S3"
        >>> ledger.clear("COM3")
    """

    def __init__(self, ledger_path: Path | None = None):
        """Initialize the board ledger.

        Args:
            ledger_path: Optional custom path for ledger file.
                        Defaults to ~/.fbuild/board_ledger.json
        """
        if ledger_path is None:
            self._ledger_path = Path.home() / ".fbuild" / "board_ledger.json"
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

    def _read_ledger(self) -> LedgerData:
        """Read the ledger file.

        Returns:
            LedgerData instance with all ledger entries
        """
        if not self._ledger_path.exists():
            return LedgerData(entries={})

        try:
            with open(self._ledger_path, encoding="utf-8") as f:
                data = json.load(f)
                if not isinstance(data, dict):
                    return LedgerData(entries={})
                return LedgerData.from_dict(data)
        except (json.JSONDecodeError, OSError):
            return LedgerData(entries={})

    def _write_ledger(self, data: LedgerData) -> None:
        """Write the ledger file.

        Args:
            data: LedgerData instance with all ledger entries
        """
        self._ensure_directory()
        try:
            with open(self._ledger_path, "w", encoding="utf-8") as f:
                json.dump(data.to_dict(), f, indent=2)
        except OSError as e:
            raise BoardLedgerError(f"Failed to write ledger: {e}") from e

    def get_chip(self, port: str) -> str | None:
        """Get the cached chip type for a port.

        Args:
            port: Serial port name (e.g., "COM3", "/dev/ttyUSB0")

        Returns:
            Chip type string (e.g., "ESP32-S3") or None if not found/stale
        """
        with self._lock:
            ledger_data = self._read_ledger()
            entry = ledger_data.entries.get(port)
            if entry is None:
                return None

            if entry.is_stale():
                return None

            return entry.chip_type

    def set_chip(self, port: str, chip_type: str) -> None:
        """Set the chip type for a port.

        Args:
            port: Serial port name (e.g., "COM3", "/dev/ttyUSB0")
            chip_type: Chip type (e.g., "ESP32-S3", "ESP32-C6")

        Raises:
            BoardLedgerError: If chip_type is not valid
        """
        # Normalize chip type (handle lowercase/variant formats)
        normalized = self._normalize_chip_type(chip_type)
        if normalized not in VALID_CHIP_TYPES:
            raise BoardLedgerError(f"Invalid chip type: {chip_type}. Valid types: {', '.join(sorted(VALID_CHIP_TYPES))}")

        entry = LedgerEntry(chip_type=normalized, timestamp=time.time())

        with self._lock:
            ledger_data = self._read_ledger()
            ledger_data.entries[port] = entry
            self._write_ledger(ledger_data)

    def clear(self, port: str) -> bool:
        """Clear the cached chip type for a port.

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
            self._write_ledger(LedgerData(entries={}))
            return count

    def clear_stale(self, threshold: float = STALE_THRESHOLD_SECONDS) -> int:
        """Remove all stale entries from the ledger.

        Args:
            threshold: Maximum age in seconds before entry is considered stale

        Returns:
            Number of entries removed
        """
        with self._lock:
            ledger_data = self._read_ledger()
            original_count = len(ledger_data.entries)

            # Filter out stale entries
            fresh_entries = {}
            for port, entry in ledger_data.entries.items():
                if not entry.is_stale(threshold):
                    fresh_entries[port] = entry

            self._write_ledger(LedgerData(entries=fresh_entries))
            return original_count - len(fresh_entries)

    def get_all(self) -> dict[str, LedgerEntry]:
        """Get all non-stale entries in the ledger.

        Returns:
            Dictionary mapping port names to LedgerEntry objects
        """
        with self._lock:
            ledger_data = self._read_ledger()
            result = {}
            for port, entry in ledger_data.entries.items():
                if not entry.is_stale():
                    result[port] = entry
            return result

    def get_environment(self, port: str) -> str | None:
        """Get the environment name for a cached port.

        Args:
            port: Serial port name

        Returns:
            Environment name (e.g., "esp32s3") or None if not found
        """
        chip_type = self.get_chip(port)
        if chip_type is None:
            return None
        return CHIP_TO_ENVIRONMENT.get(chip_type)

    @staticmethod
    def _normalize_chip_type(chip_type: str) -> str:
        """Normalize chip type to standard format.

        Args:
            chip_type: Raw chip type string (e.g., "esp32s3", "ESP32-S3")

        Returns:
            Normalized chip type (e.g., "ESP32-S3")
        """
        # Map common variations to standard format
        upper = chip_type.upper().replace("_", "-")

        # Handle formats without hyphen (esp32s3 -> ESP32-S3)
        mappings = {
            "ESP32S2": "ESP32-S2",
            "ESP32S3": "ESP32-S3",
            "ESP32C2": "ESP32-C2",
            "ESP32C3": "ESP32-C3",
            "ESP32C6": "ESP32-C6",
            "ESP32H2": "ESP32-H2",
        }

        return mappings.get(upper, upper)


@dataclass
class DetectionResult:
    """Result of chip detection.

    Attributes:
        chip_type: The detected chip type (e.g., "ESP32-S3")
        environment: The environment name (e.g., "esp32s3")
        was_cached: Whether the result came from cache
    """

    chip_type: str
    environment: str
    was_cached: bool


def detect_chip_with_esptool(port: str, verbose: bool = False) -> str:
    """Detect chip type using esptool.

    Args:
        port: Serial port to detect chip on
        verbose: Whether to show verbose output

    Returns:
        Chip type string (e.g., "ESP32-S3")

    Raises:
        ChipDetectionError: If detection fails
    """
    try:
        # Build esptool command
        cmd = [
            sys.executable,
            "-m",
            "esptool",
            "--port",
            port,
            "chip_id",
        ]

        if verbose:
            print(f"Running: {' '.join(cmd)}")

        # Set up environment (strip MSYS paths on Windows)
        env = os.environ.copy()
        if sys.platform == "win32" and "PATH" in env:
            paths = env["PATH"].split(os.pathsep)
            filtered_paths = [p for p in paths if "msys" not in p.lower()]
            env["PATH"] = os.pathsep.join(filtered_paths)

        result = safe_run(
            cmd,
            capture_output=True,
            text=True,
            timeout=30,
            env=env,
        )

        if result.returncode != 0:
            error_msg = result.stderr.strip() if result.stderr else "Unknown error"
            raise ChipDetectionError(f"esptool chip_id failed: {error_msg}")

        # Parse output to find chip type
        # Example output: "Chip is ESP32-S3 (revision v0.2)"
        output = result.stdout
        for line in output.splitlines():
            if "Chip is" in line:
                # Extract chip name
                # Format: "Chip is ESP32-S3 (revision ...)"
                parts = line.split("Chip is")
                if len(parts) >= 2:
                    chip_part = parts[1].strip()
                    # Remove revision info if present
                    if "(" in chip_part:
                        chip_part = chip_part.split("(")[0].strip()
                    return BoardLedger._normalize_chip_type(chip_part)

        raise ChipDetectionError(f"Could not parse chip type from esptool output: {output}")

    except subprocess.TimeoutExpired:
        raise ChipDetectionError(f"Chip detection timed out on port {port}")
    except KeyboardInterrupt as ke:
        from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

        handle_keyboard_interrupt_properly(ke)
    except ChipDetectionError:
        raise
    except Exception as e:
        raise ChipDetectionError(f"Chip detection failed: {e}") from e


def detect_and_cache(
    port: str,
    ledger: BoardLedger | None = None,
    force_detect: bool = False,
    verbose: bool = False,
) -> DetectionResult:
    """Detect chip type, using cache when available.

    This function first checks the ledger for a cached chip type. If not found
    or stale (or force_detect is True), it calls esptool to detect the chip
    and caches the result.

    Args:
        port: Serial port to detect chip on
        ledger: BoardLedger instance (creates default if None)
        force_detect: If True, always call esptool even if cache exists
        verbose: Whether to show verbose output

    Returns:
        DetectionResult with chip_type, environment, and was_cached flag

    Raises:
        ChipDetectionError: If detection fails
        BoardLedgerError: If caching fails
    """
    if ledger is None:
        ledger = BoardLedger()

    # Check cache first (unless force_detect)
    if not force_detect:
        cached_chip = ledger.get_chip(port)
        if cached_chip is not None:
            environment = CHIP_TO_ENVIRONMENT.get(cached_chip)
            if environment is not None:
                if verbose:
                    print(f"Using cached chip type: {cached_chip} -> {environment}")
                return DetectionResult(
                    chip_type=cached_chip,
                    environment=environment,
                    was_cached=True,
                )

    # Detect with esptool
    if verbose:
        print(f"Detecting chip on port {port}...")

    chip_type = detect_chip_with_esptool(port, verbose=verbose)
    environment = CHIP_TO_ENVIRONMENT.get(chip_type)

    if environment is None:
        raise ChipDetectionError(f"Unknown chip type: {chip_type}. Supported chips: {', '.join(sorted(VALID_CHIP_TYPES))}")

    # Cache the result
    try:
        ledger.set_chip(port, chip_type)
        if verbose:
            print(f"Cached chip detection: {port} -> {chip_type} ({environment})")
    except BoardLedgerError as e:
        if verbose:
            print(f"Warning: Failed to cache chip detection: {e}")

    return DetectionResult(
        chip_type=chip_type,
        environment=environment,
        was_cached=False,
    )


def get_environment_for_chip(chip_type: str) -> str | None:
    """Get the environment name for a chip type.

    Args:
        chip_type: Chip type string (e.g., "ESP32-S3", "esp32s3")

    Returns:
        Environment name (e.g., "esp32s3") or None if unknown
    """
    normalized = BoardLedger._normalize_chip_type(chip_type)
    return CHIP_TO_ENVIRONMENT.get(normalized)
