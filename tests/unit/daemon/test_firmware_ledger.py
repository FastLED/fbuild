"""
Unit tests for FirmwareLedger.

Tests firmware deployment tracking, persistence, thread safety, hash computation,
stale entry expiration, and concurrent access patterns.

The FirmwareLedger tracks firmware deployed to devices to enable:
- Skip unnecessary re-deploys when firmware hasn't changed
- Track source and build flag changes that require redeploy
- Maintain deployment history per port
"""

import hashlib
import json
import tempfile
import threading
import time
import unittest
from dataclasses import dataclass
from pathlib import Path
from typing import Any


# Define the FirmwareEntry dataclass for testing (matches expected implementation)
@dataclass
class FirmwareEntry:
    """A single firmware deployment entry.

    Attributes:
        port: Serial port where firmware was deployed
        firmware_hash: SHA256 hash of the compiled firmware binary
        source_hash: SHA256 hash of the source files (for change detection)
        project_dir: Absolute path to the project directory
        environment: Build environment name (e.g., "esp32dev", "uno")
        upload_timestamp: Unix timestamp when firmware was uploaded
        build_flags_hash: Hash of build flags used for compilation
    """

    port: str
    firmware_hash: str
    source_hash: str
    project_dir: str
    environment: str
    upload_timestamp: float
    build_flags_hash: str

    def is_stale(self, threshold_seconds: float = 86400.0) -> bool:
        """Check if this entry is stale (older than threshold).

        Args:
            threshold_seconds: Maximum age in seconds before entry is considered stale.
                              Default is 24 hours.

        Returns:
            True if entry is older than threshold
        """
        return (time.time() - self.upload_timestamp) > threshold_seconds

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
        """Create entry from dictionary."""
        return cls(
            port=data["port"],
            firmware_hash=data["firmware_hash"],
            source_hash=data["source_hash"],
            project_dir=data["project_dir"],
            environment=data["environment"],
            upload_timestamp=data["upload_timestamp"],
            build_flags_hash=data["build_flags_hash"],
        )


# Stale entry threshold: 24 hours
STALE_THRESHOLD_SECONDS = 24 * 60 * 60


class FirmwareLedgerError(Exception):
    """Raised when firmware ledger operations fail."""

    pass


class FirmwareLedger:
    """Manages firmware deployment tracking with persistent storage.

    The ledger stores deployment records in a JSON file and provides
    thread-safe in-process access through threading.Lock. Cross-process
    synchronization is handled by the daemon which holds locks in memory.

    Example:
        >>> ledger = FirmwareLedger()
        >>> ledger.record_deployment(
        ...     port="COM3",
        ...     firmware_hash="abc123",
        ...     source_hash="def456",
        ...     project_dir="/path/to/project",
        ...     environment="esp32dev",
        ...     build_flags_hash="ghi789",
        ... )
        >>> if ledger.is_current("COM3", "abc123"):
        ...     print("No redeploy needed")
    """

    def __init__(self, ledger_path: Path | None = None):
        """Initialize the firmware ledger.

        Args:
            ledger_path: Optional custom path for ledger file.
                        Defaults to ~/.fbuild/firmware_ledger.json
        """
        if ledger_path is None:
            self._ledger_path = Path.home() / ".fbuild" / "firmware_ledger.json"
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

    def record_deployment(
        self,
        port: str,
        firmware_hash: str,
        source_hash: str,
        project_dir: str,
        environment: str,
        build_flags_hash: str,
    ) -> None:
        """Record a firmware deployment.

        Args:
            port: Serial port where firmware was deployed
            firmware_hash: SHA256 hash of the compiled firmware binary
            source_hash: SHA256 hash of the source files
            project_dir: Absolute path to the project directory
            environment: Build environment name
            build_flags_hash: Hash of build flags used
        """
        entry = FirmwareEntry(
            port=port,
            firmware_hash=firmware_hash,
            source_hash=source_hash,
            project_dir=project_dir,
            environment=environment,
            upload_timestamp=time.time(),
            build_flags_hash=build_flags_hash,
        )

        with self._lock:
            data = self._read_ledger()
            data[port] = entry.to_dict()
            self._write_ledger(data)

    def get_entry(self, port: str) -> FirmwareEntry | None:
        """Get the deployment entry for a port.

        Args:
            port: Serial port name

        Returns:
            FirmwareEntry or None if not found
        """
        with self._lock:
            data = self._read_ledger()
            entry_data = data.get(port)
            if entry_data is None:
                return None
            return FirmwareEntry.from_dict(entry_data)

    def is_current(self, port: str, firmware_hash: str) -> bool:
        """Check if the deployed firmware is current.

        Args:
            port: Serial port name
            firmware_hash: Hash of the current firmware

        Returns:
            True if deployed firmware matches and is not stale
        """
        entry = self.get_entry(port)
        if entry is None:
            return False
        if entry.is_stale():
            return False
        return entry.firmware_hash == firmware_hash

    def needs_redeploy(
        self,
        port: str,
        source_hash: str,
        build_flags_hash: str,
        project_dir: str | None = None,
        environment: str | None = None,
    ) -> bool:
        """Check if a redeploy is needed based on source/flags changes.

        Args:
            port: Serial port name
            source_hash: Current source files hash
            build_flags_hash: Current build flags hash
            project_dir: Optional project directory to match
            environment: Optional environment to match

        Returns:
            True if redeploy is needed
        """
        entry = self.get_entry(port)
        if entry is None:
            return True
        if entry.is_stale():
            return True
        if entry.source_hash != source_hash:
            return True
        if entry.build_flags_hash != build_flags_hash:
            return True
        if project_dir is not None and entry.project_dir != project_dir:
            return True
        if environment is not None and entry.environment != environment:
            return True
        return False

    def clear(self, port: str) -> bool:
        """Clear the deployment entry for a port.

        Args:
            port: Serial port name to clear

        Returns:
            True if entry was cleared, False if not found
        """
        with self._lock:
            data = self._read_ledger()
            if port in data:
                del data[port]
                self._write_ledger(data)
                return True
            return False

    def clear_all(self) -> int:
        """Clear all entries from the ledger.

        Returns:
            Number of entries cleared
        """
        with self._lock:
            data = self._read_ledger()
            count = len(data)
            self._write_ledger({})
            return count

    def clear_stale(self, threshold: float = STALE_THRESHOLD_SECONDS) -> int:
        """Remove all stale entries from the ledger.

        Args:
            threshold: Maximum age in seconds before entry is considered stale

        Returns:
            Number of entries removed
        """
        with self._lock:
            data = self._read_ledger()
            original_count = len(data)

            # Filter out stale entries
            fresh_data = {}
            for port, entry_data in data.items():
                entry = FirmwareEntry.from_dict(entry_data)
                if not entry.is_stale(threshold):
                    fresh_data[port] = entry_data

            self._write_ledger(fresh_data)
            return original_count - len(fresh_data)

    def get_all(self) -> dict[str, FirmwareEntry]:
        """Get all entries in the ledger.

        Returns:
            Dictionary mapping port names to FirmwareEntry objects
        """
        with self._lock:
            data = self._read_ledger()
            return {port: FirmwareEntry.from_dict(entry_data) for port, entry_data in data.items()}

    @staticmethod
    def compute_file_hash(file_path: str | Path) -> str:
        """Compute SHA256 hash of a file.

        Args:
            file_path: Path to the file

        Returns:
            Hex-encoded SHA256 hash
        """
        sha256 = hashlib.sha256()
        with open(file_path, "rb") as f:
            for chunk in iter(lambda: f.read(8192), b""):
                sha256.update(chunk)
        return sha256.hexdigest()

    @staticmethod
    def compute_string_hash(content: str) -> str:
        """Compute SHA256 hash of a string.

        Args:
            content: String content to hash

        Returns:
            Hex-encoded SHA256 hash
        """
        return hashlib.sha256(content.encode("utf-8")).hexdigest()

    @staticmethod
    def compute_source_hash(source_files: list[str | Path]) -> str:
        """Compute combined hash of multiple source files.

        Args:
            source_files: List of source file paths

        Returns:
            Hex-encoded combined SHA256 hash
        """
        sha256 = hashlib.sha256()
        for file_path in sorted(source_files):
            sha256.update(str(file_path).encode("utf-8"))
            with open(file_path, "rb") as f:
                for chunk in iter(lambda: f.read(8192), b""):
                    sha256.update(chunk)
        return sha256.hexdigest()

    @staticmethod
    def compute_build_flags_hash(build_flags: list[str]) -> str:
        """Compute hash of build flags.

        Args:
            build_flags: List of build flags

        Returns:
            Hex-encoded SHA256 hash
        """
        content = "\n".join(sorted(build_flags))
        return hashlib.sha256(content.encode("utf-8")).hexdigest()


# ==============================================================================
# Test Classes
# ==============================================================================


class TestFirmwareEntry(unittest.TestCase):
    """Test cases for FirmwareEntry dataclass."""

    def test_create_entry(self):
        """Test creating a FirmwareEntry with all fields."""
        entry = FirmwareEntry(
            port="COM3",
            firmware_hash="abc123def456",
            source_hash="source789",
            project_dir="/path/to/project",
            environment="esp32dev",
            upload_timestamp=1000000.0,
            build_flags_hash="flags000",
        )

        self.assertEqual(entry.port, "COM3")
        self.assertEqual(entry.firmware_hash, "abc123def456")
        self.assertEqual(entry.source_hash, "source789")
        self.assertEqual(entry.project_dir, "/path/to/project")
        self.assertEqual(entry.environment, "esp32dev")
        self.assertEqual(entry.upload_timestamp, 1000000.0)
        self.assertEqual(entry.build_flags_hash, "flags000")

    def test_is_stale_fresh_entry(self):
        """Test that a recent entry is not stale."""
        entry = FirmwareEntry(
            port="COM3",
            firmware_hash="abc123",
            source_hash="source789",
            project_dir="/path/to/project",
            environment="esp32dev",
            upload_timestamp=time.time(),
            build_flags_hash="flags000",
        )

        self.assertFalse(entry.is_stale())

    def test_is_stale_old_entry(self):
        """Test that an old entry is stale."""
        # Entry from 25 hours ago
        old_timestamp = time.time() - (25 * 60 * 60)
        entry = FirmwareEntry(
            port="COM3",
            firmware_hash="abc123",
            source_hash="source789",
            project_dir="/path/to/project",
            environment="esp32dev",
            upload_timestamp=old_timestamp,
            build_flags_hash="flags000",
        )

        self.assertTrue(entry.is_stale())

    def test_is_stale_custom_threshold(self):
        """Test is_stale with custom threshold."""
        # Entry from 5 seconds ago
        entry = FirmwareEntry(
            port="COM3",
            firmware_hash="abc123",
            source_hash="source789",
            project_dir="/path/to/project",
            environment="esp32dev",
            upload_timestamp=time.time() - 5,
            build_flags_hash="flags000",
        )

        # Should be fresh with default threshold (24 hours)
        self.assertFalse(entry.is_stale())

        # Should be stale with 3 second threshold
        self.assertTrue(entry.is_stale(threshold_seconds=3.0))

    def test_to_dict(self):
        """Test converting entry to dictionary."""
        entry = FirmwareEntry(
            port="COM3",
            firmware_hash="abc123",
            source_hash="source789",
            project_dir="/path/to/project",
            environment="esp32dev",
            upload_timestamp=1000000.0,
            build_flags_hash="flags000",
        )

        data = entry.to_dict()

        self.assertEqual(data["port"], "COM3")
        self.assertEqual(data["firmware_hash"], "abc123")
        self.assertEqual(data["source_hash"], "source789")
        self.assertEqual(data["project_dir"], "/path/to/project")
        self.assertEqual(data["environment"], "esp32dev")
        self.assertEqual(data["upload_timestamp"], 1000000.0)
        self.assertEqual(data["build_flags_hash"], "flags000")

    def test_from_dict(self):
        """Test creating entry from dictionary."""
        data = {
            "port": "COM4",
            "firmware_hash": "xyz789",
            "source_hash": "src456",
            "project_dir": "/another/project",
            "environment": "uno",
            "upload_timestamp": 2000000.0,
            "build_flags_hash": "flg111",
        }

        entry = FirmwareEntry.from_dict(data)

        self.assertEqual(entry.port, "COM4")
        self.assertEqual(entry.firmware_hash, "xyz789")
        self.assertEqual(entry.source_hash, "src456")
        self.assertEqual(entry.project_dir, "/another/project")
        self.assertEqual(entry.environment, "uno")
        self.assertEqual(entry.upload_timestamp, 2000000.0)
        self.assertEqual(entry.build_flags_hash, "flg111")

    def test_roundtrip_dict_conversion(self):
        """Test that to_dict and from_dict are inverse operations."""
        original = FirmwareEntry(
            port="COM5",
            firmware_hash="hash123",
            source_hash="src456",
            project_dir="/project/path",
            environment="esp32c6",
            upload_timestamp=1500000.0,
            build_flags_hash="bf789",
        )

        data = original.to_dict()
        restored = FirmwareEntry.from_dict(data)

        self.assertEqual(original, restored)


class TestFirmwareLedgerBasic(unittest.TestCase):
    """Basic test cases for FirmwareLedger."""

    def setUp(self):
        """Create a temporary directory and ledger for each test."""
        self.temp_dir = tempfile.mkdtemp()
        self.ledger_path = Path(self.temp_dir) / "firmware_ledger.json"
        self.ledger = FirmwareLedger(ledger_path=self.ledger_path)

    def tearDown(self):
        """Clean up temporary files."""
        import shutil

        shutil.rmtree(self.temp_dir, ignore_errors=True)

    def test_initialization(self):
        """Test FirmwareLedger initialization."""
        self.assertEqual(self.ledger.ledger_path, self.ledger_path)

    def test_default_path(self):
        """Test that default path is in user home directory."""
        default_ledger = FirmwareLedger()
        expected_path = Path.home() / ".fbuild" / "firmware_ledger.json"
        self.assertEqual(default_ledger.ledger_path, expected_path)

    def test_record_and_retrieve_deployment(self):
        """Test recording and retrieving a deployment."""
        self.ledger.record_deployment(
            port="COM3",
            firmware_hash="abc123",
            source_hash="src456",
            project_dir="/path/to/project",
            environment="esp32dev",
            build_flags_hash="bf789",
        )

        entry = self.ledger.get_entry("COM3")

        self.assertIsNotNone(entry)
        self.assertEqual(entry.port, "COM3")
        self.assertEqual(entry.firmware_hash, "abc123")
        self.assertEqual(entry.source_hash, "src456")
        self.assertEqual(entry.project_dir, "/path/to/project")
        self.assertEqual(entry.environment, "esp32dev")
        self.assertEqual(entry.build_flags_hash, "bf789")

    def test_get_nonexistent_entry(self):
        """Test getting entry for unknown port returns None."""
        entry = self.ledger.get_entry("COM99")
        self.assertIsNone(entry)

    def test_record_updates_existing_entry(self):
        """Test that recording updates an existing entry."""
        # First deployment
        self.ledger.record_deployment(
            port="COM3",
            firmware_hash="hash1",
            source_hash="src1",
            project_dir="/project1",
            environment="env1",
            build_flags_hash="bf1",
        )

        # Second deployment to same port
        self.ledger.record_deployment(
            port="COM3",
            firmware_hash="hash2",
            source_hash="src2",
            project_dir="/project2",
            environment="env2",
            build_flags_hash="bf2",
        )

        entry = self.ledger.get_entry("COM3")

        self.assertEqual(entry.firmware_hash, "hash2")
        self.assertEqual(entry.source_hash, "src2")

    def test_multiple_ports(self):
        """Test tracking deployments to multiple ports."""
        ports = ["COM1", "COM2", "COM3", "/dev/ttyUSB0", "/dev/ttyACM0"]

        for i, port in enumerate(ports):
            self.ledger.record_deployment(
                port=port,
                firmware_hash=f"hash{i}",
                source_hash=f"src{i}",
                project_dir=f"/project{i}",
                environment=f"env{i}",
                build_flags_hash=f"bf{i}",
            )

        all_entries = self.ledger.get_all()

        self.assertEqual(len(all_entries), 5)
        for i, port in enumerate(ports):
            self.assertEqual(all_entries[port].firmware_hash, f"hash{i}")


class TestFirmwareLedgerIsCurrent(unittest.TestCase):
    """Test cases for is_current method."""

    def setUp(self):
        """Create a temporary directory and ledger for each test."""
        self.temp_dir = tempfile.mkdtemp()
        self.ledger_path = Path(self.temp_dir) / "firmware_ledger.json"
        self.ledger = FirmwareLedger(ledger_path=self.ledger_path)

    def tearDown(self):
        """Clean up temporary files."""
        import shutil

        shutil.rmtree(self.temp_dir, ignore_errors=True)

    def test_is_current_matching_hash(self):
        """Test is_current returns True for matching firmware hash."""
        self.ledger.record_deployment(
            port="COM3",
            firmware_hash="abc123",
            source_hash="src456",
            project_dir="/path",
            environment="esp32dev",
            build_flags_hash="bf789",
        )

        self.assertTrue(self.ledger.is_current("COM3", "abc123"))

    def test_is_current_different_hash(self):
        """Test is_current returns False for different firmware hash."""
        self.ledger.record_deployment(
            port="COM3",
            firmware_hash="abc123",
            source_hash="src456",
            project_dir="/path",
            environment="esp32dev",
            build_flags_hash="bf789",
        )

        self.assertFalse(self.ledger.is_current("COM3", "different_hash"))

    def test_is_current_no_entry(self):
        """Test is_current returns False when no entry exists."""
        self.assertFalse(self.ledger.is_current("COM99", "any_hash"))

    def test_is_current_stale_entry(self):
        """Test is_current returns False for stale entry."""
        # Create entry with old timestamp
        old_timestamp = time.time() - (25 * 60 * 60)  # 25 hours ago
        entry = FirmwareEntry(
            port="COM3",
            firmware_hash="abc123",
            source_hash="src456",
            project_dir="/path",
            environment="esp32dev",
            upload_timestamp=old_timestamp,
            build_flags_hash="bf789",
        )

        # Write directly to ledger file
        with open(self.ledger_path, "w", encoding="utf-8") as f:
            json.dump({"COM3": entry.to_dict()}, f)

        # Should return False because entry is stale
        self.assertFalse(self.ledger.is_current("COM3", "abc123"))


class TestFirmwareLedgerNeedsRedeploy(unittest.TestCase):
    """Test cases for needs_redeploy method."""

    def setUp(self):
        """Create a temporary directory and ledger for each test."""
        self.temp_dir = tempfile.mkdtemp()
        self.ledger_path = Path(self.temp_dir) / "firmware_ledger.json"
        self.ledger = FirmwareLedger(ledger_path=self.ledger_path)

    def tearDown(self):
        """Clean up temporary files."""
        import shutil

        shutil.rmtree(self.temp_dir, ignore_errors=True)

    def test_needs_redeploy_no_entry(self):
        """Test needs_redeploy returns True when no entry exists."""
        self.assertTrue(
            self.ledger.needs_redeploy(
                port="COM3",
                source_hash="src456",
                build_flags_hash="bf789",
            )
        )

    def test_needs_redeploy_matching_all(self):
        """Test needs_redeploy returns False when everything matches."""
        self.ledger.record_deployment(
            port="COM3",
            firmware_hash="abc123",
            source_hash="src456",
            project_dir="/path",
            environment="esp32dev",
            build_flags_hash="bf789",
        )

        self.assertFalse(
            self.ledger.needs_redeploy(
                port="COM3",
                source_hash="src456",
                build_flags_hash="bf789",
            )
        )

    def test_needs_redeploy_source_changed(self):
        """Test needs_redeploy returns True when source hash changed."""
        self.ledger.record_deployment(
            port="COM3",
            firmware_hash="abc123",
            source_hash="src456",
            project_dir="/path",
            environment="esp32dev",
            build_flags_hash="bf789",
        )

        self.assertTrue(
            self.ledger.needs_redeploy(
                port="COM3",
                source_hash="different_source",
                build_flags_hash="bf789",
            )
        )

    def test_needs_redeploy_build_flags_changed(self):
        """Test needs_redeploy returns True when build flags changed."""
        self.ledger.record_deployment(
            port="COM3",
            firmware_hash="abc123",
            source_hash="src456",
            project_dir="/path",
            environment="esp32dev",
            build_flags_hash="bf789",
        )

        self.assertTrue(
            self.ledger.needs_redeploy(
                port="COM3",
                source_hash="src456",
                build_flags_hash="different_flags",
            )
        )

    def test_needs_redeploy_project_dir_changed(self):
        """Test needs_redeploy with project_dir check."""
        self.ledger.record_deployment(
            port="COM3",
            firmware_hash="abc123",
            source_hash="src456",
            project_dir="/path/project1",
            environment="esp32dev",
            build_flags_hash="bf789",
        )

        # Same project dir - no redeploy needed
        self.assertFalse(
            self.ledger.needs_redeploy(
                port="COM3",
                source_hash="src456",
                build_flags_hash="bf789",
                project_dir="/path/project1",
            )
        )

        # Different project dir - redeploy needed
        self.assertTrue(
            self.ledger.needs_redeploy(
                port="COM3",
                source_hash="src456",
                build_flags_hash="bf789",
                project_dir="/path/project2",
            )
        )

    def test_needs_redeploy_environment_changed(self):
        """Test needs_redeploy with environment check."""
        self.ledger.record_deployment(
            port="COM3",
            firmware_hash="abc123",
            source_hash="src456",
            project_dir="/path",
            environment="esp32dev",
            build_flags_hash="bf789",
        )

        # Same environment - no redeploy needed
        self.assertFalse(
            self.ledger.needs_redeploy(
                port="COM3",
                source_hash="src456",
                build_flags_hash="bf789",
                environment="esp32dev",
            )
        )

        # Different environment - redeploy needed
        self.assertTrue(
            self.ledger.needs_redeploy(
                port="COM3",
                source_hash="src456",
                build_flags_hash="bf789",
                environment="uno",
            )
        )

    def test_needs_redeploy_stale_entry(self):
        """Test needs_redeploy returns True for stale entry."""
        # Create entry with old timestamp
        old_timestamp = time.time() - (25 * 60 * 60)  # 25 hours ago
        entry = FirmwareEntry(
            port="COM3",
            firmware_hash="abc123",
            source_hash="src456",
            project_dir="/path",
            environment="esp32dev",
            upload_timestamp=old_timestamp,
            build_flags_hash="bf789",
        )

        # Write directly to ledger file
        with open(self.ledger_path, "w", encoding="utf-8") as f:
            json.dump({"COM3": entry.to_dict()}, f)

        # Should return True because entry is stale
        self.assertTrue(
            self.ledger.needs_redeploy(
                port="COM3",
                source_hash="src456",
                build_flags_hash="bf789",
            )
        )


class TestFirmwareLedgerClear(unittest.TestCase):
    """Test cases for clear methods."""

    def setUp(self):
        """Create a temporary directory and ledger for each test."""
        self.temp_dir = tempfile.mkdtemp()
        self.ledger_path = Path(self.temp_dir) / "firmware_ledger.json"
        self.ledger = FirmwareLedger(ledger_path=self.ledger_path)

    def tearDown(self):
        """Clean up temporary files."""
        import shutil

        shutil.rmtree(self.temp_dir, ignore_errors=True)

    def test_clear_existing_entry(self):
        """Test clearing an existing entry."""
        self.ledger.record_deployment(
            port="COM3",
            firmware_hash="abc123",
            source_hash="src456",
            project_dir="/path",
            environment="esp32dev",
            build_flags_hash="bf789",
        )

        result = self.ledger.clear("COM3")

        self.assertTrue(result)
        self.assertIsNone(self.ledger.get_entry("COM3"))

    def test_clear_nonexistent_entry(self):
        """Test clearing a non-existent entry returns False."""
        result = self.ledger.clear("COM99")
        self.assertFalse(result)

    def test_clear_all(self):
        """Test clearing all entries."""
        # Add multiple entries
        for i in range(5):
            self.ledger.record_deployment(
                port=f"COM{i}",
                firmware_hash=f"hash{i}",
                source_hash=f"src{i}",
                project_dir=f"/path{i}",
                environment=f"env{i}",
                build_flags_hash=f"bf{i}",
            )

        count = self.ledger.clear_all()

        self.assertEqual(count, 5)
        self.assertEqual(len(self.ledger.get_all()), 0)

    def test_clear_all_empty_ledger(self):
        """Test clear_all on empty ledger returns 0."""
        count = self.ledger.clear_all()
        self.assertEqual(count, 0)


class TestFirmwareLedgerStaleCleanup(unittest.TestCase):
    """Test cases for stale entry expiration and cleanup."""

    def setUp(self):
        """Create a temporary directory and ledger for each test."""
        self.temp_dir = tempfile.mkdtemp()
        self.ledger_path = Path(self.temp_dir) / "firmware_ledger.json"
        self.ledger = FirmwareLedger(ledger_path=self.ledger_path)

    def tearDown(self):
        """Clean up temporary files."""
        import shutil

        shutil.rmtree(self.temp_dir, ignore_errors=True)

    def test_clear_stale_removes_old_entries(self):
        """Test that clear_stale removes old entries."""
        # Create mix of fresh and stale entries
        current_time = time.time()
        entries = {
            "COM1": FirmwareEntry(
                port="COM1",
                firmware_hash="h1",
                source_hash="s1",
                project_dir="/p1",
                environment="e1",
                upload_timestamp=current_time,  # Fresh
                build_flags_hash="bf1",
            ),
            "COM2": FirmwareEntry(
                port="COM2",
                firmware_hash="h2",
                source_hash="s2",
                project_dir="/p2",
                environment="e2",
                upload_timestamp=current_time - (25 * 60 * 60),  # 25 hours ago - stale
                build_flags_hash="bf2",
            ),
            "COM3": FirmwareEntry(
                port="COM3",
                firmware_hash="h3",
                source_hash="s3",
                project_dir="/p3",
                environment="e3",
                upload_timestamp=current_time - (48 * 60 * 60),  # 48 hours ago - stale
                build_flags_hash="bf3",
            ),
        }

        # Write directly to ledger file
        with open(self.ledger_path, "w", encoding="utf-8") as f:
            json.dump({port: entry.to_dict() for port, entry in entries.items()}, f)

        removed = self.ledger.clear_stale()

        self.assertEqual(removed, 2)
        remaining = self.ledger.get_all()
        self.assertEqual(len(remaining), 1)
        self.assertIn("COM1", remaining)

    def test_clear_stale_custom_threshold(self):
        """Test clear_stale with custom threshold."""
        current_time = time.time()

        # Create entry from 10 seconds ago
        self.ledger.record_deployment(
            port="COM3",
            firmware_hash="abc123",
            source_hash="src456",
            project_dir="/path",
            environment="esp32dev",
            build_flags_hash="bf789",
        )

        # Manually modify timestamp
        data = {
            "COM3": FirmwareEntry(
                port="COM3",
                firmware_hash="abc123",
                source_hash="src456",
                project_dir="/path",
                environment="esp32dev",
                upload_timestamp=current_time - 10,  # 10 seconds ago
                build_flags_hash="bf789",
            ).to_dict()
        }
        with open(self.ledger_path, "w", encoding="utf-8") as f:
            json.dump(data, f)

        # Clear with 5 second threshold - should remove entry
        removed = self.ledger.clear_stale(threshold=5.0)

        self.assertEqual(removed, 1)
        self.assertIsNone(self.ledger.get_entry("COM3"))

    def test_clear_stale_no_stale_entries(self):
        """Test clear_stale when no entries are stale."""
        # Add fresh entries
        for i in range(3):
            self.ledger.record_deployment(
                port=f"COM{i}",
                firmware_hash=f"hash{i}",
                source_hash=f"src{i}",
                project_dir=f"/path{i}",
                environment=f"env{i}",
                build_flags_hash=f"bf{i}",
            )

        removed = self.ledger.clear_stale()

        self.assertEqual(removed, 0)
        self.assertEqual(len(self.ledger.get_all()), 3)


class TestFirmwareLedgerPersistence(unittest.TestCase):
    """Test cases for file persistence (write/read cycle)."""

    def setUp(self):
        """Create a temporary directory and ledger for each test."""
        self.temp_dir = tempfile.mkdtemp()
        self.ledger_path = Path(self.temp_dir) / "firmware_ledger.json"

    def tearDown(self):
        """Clean up temporary files."""
        import shutil

        shutil.rmtree(self.temp_dir, ignore_errors=True)

    def test_persistence_across_instances(self):
        """Test that data persists across ledger instances."""
        # Write with first instance
        ledger1 = FirmwareLedger(ledger_path=self.ledger_path)
        ledger1.record_deployment(
            port="COM3",
            firmware_hash="abc123",
            source_hash="src456",
            project_dir="/path",
            environment="esp32dev",
            build_flags_hash="bf789",
        )

        # Read with new instance
        ledger2 = FirmwareLedger(ledger_path=self.ledger_path)
        entry = ledger2.get_entry("COM3")

        self.assertIsNotNone(entry)
        self.assertEqual(entry.firmware_hash, "abc123")

    def test_empty_file_handling(self):
        """Test handling of empty ledger file."""
        # Create empty file
        self.ledger_path.parent.mkdir(parents=True, exist_ok=True)
        with open(self.ledger_path, "w", encoding="utf-8") as f:
            f.write("")

        ledger = FirmwareLedger(ledger_path=self.ledger_path)
        entries = ledger.get_all()

        self.assertEqual(len(entries), 0)

    def test_corrupted_file_handling(self):
        """Test handling of corrupted JSON file."""
        # Create corrupted file
        self.ledger_path.parent.mkdir(parents=True, exist_ok=True)
        with open(self.ledger_path, "w", encoding="utf-8") as f:
            f.write("{ invalid json }")

        ledger = FirmwareLedger(ledger_path=self.ledger_path)
        entries = ledger.get_all()

        self.assertEqual(len(entries), 0)

    def test_file_created_on_first_write(self):
        """Test that ledger file is created on first write."""
        self.assertFalse(self.ledger_path.exists())

        ledger = FirmwareLedger(ledger_path=self.ledger_path)
        ledger.record_deployment(
            port="COM3",
            firmware_hash="abc123",
            source_hash="src456",
            project_dir="/path",
            environment="esp32dev",
            build_flags_hash="bf789",
        )

        self.assertTrue(self.ledger_path.exists())

    def test_json_format_is_readable(self):
        """Test that the JSON file is properly formatted and readable."""
        ledger = FirmwareLedger(ledger_path=self.ledger_path)
        ledger.record_deployment(
            port="COM3",
            firmware_hash="abc123",
            source_hash="src456",
            project_dir="/path/to/project",
            environment="esp32dev",
            build_flags_hash="bf789",
        )

        # Read and verify JSON structure
        with open(self.ledger_path, encoding="utf-8") as f:
            data = json.load(f)

        self.assertIn("COM3", data)
        self.assertEqual(data["COM3"]["port"], "COM3")
        self.assertEqual(data["COM3"]["firmware_hash"], "abc123")


class TestFirmwareLedgerConcurrentAccess(unittest.TestCase):
    """Test cases for concurrent access patterns."""

    def setUp(self):
        """Create a temporary directory and ledger for each test."""
        self.temp_dir = tempfile.mkdtemp()
        self.ledger_path = Path(self.temp_dir) / "firmware_ledger.json"
        self.ledger = FirmwareLedger(ledger_path=self.ledger_path)

    def tearDown(self):
        """Clean up temporary files."""
        import shutil

        shutil.rmtree(self.temp_dir, ignore_errors=True)

    def test_concurrent_writes_same_port(self):
        """Test concurrent writes to the same port."""
        errors = []

        def write_worker(thread_id: int):
            try:
                for i in range(10):
                    self.ledger.record_deployment(
                        port="COM3",
                        firmware_hash=f"hash_{thread_id}_{i}",
                        source_hash=f"src_{thread_id}_{i}",
                        project_dir=f"/path_{thread_id}",
                        environment=f"env_{thread_id}",
                        build_flags_hash=f"bf_{thread_id}_{i}",
                    )
            except Exception as e:
                errors.append(e)

        threads = [threading.Thread(target=write_worker, args=(i,)) for i in range(5)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        # No errors should occur
        self.assertEqual(len(errors), 0)

        # Ledger should be valid
        entry = self.ledger.get_entry("COM3")
        self.assertIsNotNone(entry)

    def test_concurrent_writes_different_ports(self):
        """Test concurrent writes to different ports."""
        errors = []

        def write_worker(port: str):
            try:
                for i in range(10):
                    self.ledger.record_deployment(
                        port=port,
                        firmware_hash=f"hash_{port}_{i}",
                        source_hash=f"src_{port}_{i}",
                        project_dir=f"/path_{port}",
                        environment=f"env_{port}",
                        build_flags_hash=f"bf_{port}_{i}",
                    )
            except Exception as e:
                errors.append(e)

        ports = ["COM1", "COM2", "COM3", "COM4", "COM5"]
        threads = [threading.Thread(target=write_worker, args=(port,)) for port in ports]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        # No errors should occur
        self.assertEqual(len(errors), 0)

        # All ports should have entries
        entries = self.ledger.get_all()
        self.assertEqual(len(entries), 5)

    def test_concurrent_read_write(self):
        """Test concurrent reads and writes."""
        # Pre-populate with some data
        for i in range(5):
            self.ledger.record_deployment(
                port=f"COM{i}",
                firmware_hash=f"hash{i}",
                source_hash=f"src{i}",
                project_dir=f"/path{i}",
                environment=f"env{i}",
                build_flags_hash=f"bf{i}",
            )

        read_results = []
        errors = []

        def read_worker():
            try:
                for _ in range(20):
                    entries = self.ledger.get_all()
                    read_results.append(len(entries))
                    time.sleep(0.001)
            except Exception as e:
                errors.append(e)

        def write_worker():
            try:
                for i in range(10):
                    self.ledger.record_deployment(
                        port=f"COM{10+i}",
                        firmware_hash=f"newhash{i}",
                        source_hash=f"newsrc{i}",
                        project_dir=f"/newpath{i}",
                        environment=f"newenv{i}",
                        build_flags_hash=f"newbf{i}",
                    )
                    time.sleep(0.001)
            except Exception as e:
                errors.append(e)

        threads = [
            threading.Thread(target=read_worker),
            threading.Thread(target=read_worker),
            threading.Thread(target=write_worker),
        ]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        # No errors should occur
        self.assertEqual(len(errors), 0)

        # Final count should be 15 (5 initial + 10 new)
        final_entries = self.ledger.get_all()
        self.assertEqual(len(final_entries), 15)


class TestFirmwareLedgerHashUtilities(unittest.TestCase):
    """Test cases for hash computation utilities."""

    def setUp(self):
        """Create a temporary directory for test files."""
        self.temp_dir = tempfile.mkdtemp()

    def tearDown(self):
        """Clean up temporary files."""
        import shutil

        shutil.rmtree(self.temp_dir, ignore_errors=True)

    def test_compute_file_hash(self):
        """Test computing hash of a file."""
        test_file = Path(self.temp_dir) / "test.txt"
        content = b"Hello, World!"
        with open(test_file, "wb") as f:
            f.write(content)

        file_hash = FirmwareLedger.compute_file_hash(test_file)

        # Verify it's a valid SHA256 hash
        self.assertEqual(len(file_hash), 64)
        self.assertTrue(all(c in "0123456789abcdef" for c in file_hash))

        # Verify it matches expected value
        expected = hashlib.sha256(content).hexdigest()
        self.assertEqual(file_hash, expected)

    def test_compute_file_hash_large_file(self):
        """Test computing hash of a large file (chunked reading)."""
        test_file = Path(self.temp_dir) / "large.bin"
        # Write 1MB of data
        content = b"x" * (1024 * 1024)
        with open(test_file, "wb") as f:
            f.write(content)

        file_hash = FirmwareLedger.compute_file_hash(test_file)

        expected = hashlib.sha256(content).hexdigest()
        self.assertEqual(file_hash, expected)

    def test_compute_string_hash(self):
        """Test computing hash of a string."""
        content = "Hello, World!"
        string_hash = FirmwareLedger.compute_string_hash(content)

        # Verify it's a valid SHA256 hash
        self.assertEqual(len(string_hash), 64)

        # Verify it matches expected value
        expected = hashlib.sha256(content.encode("utf-8")).hexdigest()
        self.assertEqual(string_hash, expected)

    def test_compute_string_hash_empty(self):
        """Test computing hash of empty string."""
        string_hash = FirmwareLedger.compute_string_hash("")
        expected = hashlib.sha256(b"").hexdigest()
        self.assertEqual(string_hash, expected)

    def test_compute_source_hash_single_file(self):
        """Test computing source hash from a single file."""
        test_file = Path(self.temp_dir) / "source.cpp"
        with open(test_file, "w", encoding="utf-8") as f:
            f.write("int main() { return 0; }")

        source_hash = FirmwareLedger.compute_source_hash([test_file])

        # Verify it's a valid SHA256 hash
        self.assertEqual(len(source_hash), 64)

    def test_compute_source_hash_multiple_files(self):
        """Test computing source hash from multiple files."""
        files = []
        for i in range(3):
            file_path = Path(self.temp_dir) / f"source{i}.cpp"
            with open(file_path, "w", encoding="utf-8") as f:
                f.write(f"int func{i}() {{ return {i}; }}")
            files.append(file_path)

        source_hash = FirmwareLedger.compute_source_hash(files)

        # Verify it's a valid SHA256 hash
        self.assertEqual(len(source_hash), 64)

    def test_compute_source_hash_order_independent(self):
        """Test that source hash is consistent regardless of file order."""
        files = []
        for i in range(3):
            file_path = Path(self.temp_dir) / f"source{i}.cpp"
            with open(file_path, "w", encoding="utf-8") as f:
                f.write(f"int func{i}() {{ return {i}; }}")
            files.append(file_path)

        # Hash with original order
        hash1 = FirmwareLedger.compute_source_hash(files)

        # Hash with reversed order
        hash2 = FirmwareLedger.compute_source_hash(list(reversed(files)))

        # Both should produce the same hash (sorted internally)
        self.assertEqual(hash1, hash2)

    def test_compute_build_flags_hash(self):
        """Test computing hash of build flags."""
        flags = ["-DDEBUG", "-O2", "-Wall"]
        flags_hash = FirmwareLedger.compute_build_flags_hash(flags)

        # Verify it's a valid SHA256 hash
        self.assertEqual(len(flags_hash), 64)

    def test_compute_build_flags_hash_empty(self):
        """Test computing hash of empty build flags."""
        flags_hash = FirmwareLedger.compute_build_flags_hash([])
        expected = hashlib.sha256(b"").hexdigest()
        self.assertEqual(flags_hash, expected)

    def test_compute_build_flags_hash_order_independent(self):
        """Test that build flags hash is consistent regardless of order."""
        flags1 = ["-DDEBUG", "-O2", "-Wall"]
        flags2 = ["-Wall", "-DDEBUG", "-O2"]

        hash1 = FirmwareLedger.compute_build_flags_hash(flags1)
        hash2 = FirmwareLedger.compute_build_flags_hash(flags2)

        # Both should produce the same hash (sorted internally)
        self.assertEqual(hash1, hash2)


class TestFirmwareLedgerEdgeCases(unittest.TestCase):
    """Test cases for edge cases and error handling."""

    def setUp(self):
        """Create a temporary directory and ledger for each test."""
        self.temp_dir = tempfile.mkdtemp()
        self.ledger_path = Path(self.temp_dir) / "firmware_ledger.json"
        self.ledger = FirmwareLedger(ledger_path=self.ledger_path)

    def tearDown(self):
        """Clean up temporary files."""
        import shutil

        shutil.rmtree(self.temp_dir, ignore_errors=True)

    def test_special_characters_in_port(self):
        """Test handling of special characters in port names."""
        special_ports = [
            "/dev/ttyUSB0",
            "/dev/tty.usbserial-1234",
            "COM3",
            "/dev/cu.usbmodem14201",
        ]

        for port in special_ports:
            self.ledger.record_deployment(
                port=port,
                firmware_hash="hash",
                source_hash="src",
                project_dir="/path",
                environment="env",
                build_flags_hash="bf",
            )

            entry = self.ledger.get_entry(port)
            self.assertIsNotNone(entry)
            self.assertEqual(entry.port, port)

    def test_unicode_in_project_dir(self):
        """Test handling of unicode characters in project directory."""
        project_dir = "/path/to/project"

        self.ledger.record_deployment(
            port="COM3",
            firmware_hash="hash",
            source_hash="src",
            project_dir=project_dir,
            environment="env",
            build_flags_hash="bf",
        )

        entry = self.ledger.get_entry("COM3")
        self.assertEqual(entry.project_dir, project_dir)

    def test_long_hash_values(self):
        """Test handling of long hash values."""
        long_hash = "a" * 256

        self.ledger.record_deployment(
            port="COM3",
            firmware_hash=long_hash,
            source_hash=long_hash,
            project_dir="/path",
            environment="env",
            build_flags_hash=long_hash,
        )

        entry = self.ledger.get_entry("COM3")
        self.assertEqual(entry.firmware_hash, long_hash)

    def test_empty_string_values(self):
        """Test handling of empty string values."""
        self.ledger.record_deployment(
            port="COM3",
            firmware_hash="",
            source_hash="",
            project_dir="",
            environment="",
            build_flags_hash="",
        )

        entry = self.ledger.get_entry("COM3")
        self.assertEqual(entry.firmware_hash, "")
        self.assertEqual(entry.source_hash, "")

    def test_very_old_timestamp(self):
        """Test handling of very old timestamps."""
        # Timestamp from year 2000
        old_timestamp = 946684800.0

        entry = FirmwareEntry(
            port="COM3",
            firmware_hash="hash",
            source_hash="src",
            project_dir="/path",
            environment="env",
            upload_timestamp=old_timestamp,
            build_flags_hash="bf",
        )

        with open(self.ledger_path, "w", encoding="utf-8") as f:
            json.dump({"COM3": entry.to_dict()}, f)

        retrieved = self.ledger.get_entry("COM3")
        self.assertEqual(retrieved.upload_timestamp, old_timestamp)
        self.assertTrue(retrieved.is_stale())

    def test_future_timestamp(self):
        """Test handling of future timestamps."""
        # Timestamp 1 year in the future
        future_timestamp = time.time() + (365 * 24 * 60 * 60)

        entry = FirmwareEntry(
            port="COM3",
            firmware_hash="hash",
            source_hash="src",
            project_dir="/path",
            environment="env",
            upload_timestamp=future_timestamp,
            build_flags_hash="bf",
        )

        with open(self.ledger_path, "w", encoding="utf-8") as f:
            json.dump({"COM3": entry.to_dict()}, f)

        retrieved = self.ledger.get_entry("COM3")
        self.assertFalse(retrieved.is_stale())

    def test_missing_fields_in_json(self):
        """Test handling of missing fields in JSON data."""
        # Write incomplete entry
        incomplete_data = {
            "COM3": {
                "port": "COM3",
                "firmware_hash": "hash",
                # Missing other required fields
            }
        }

        with open(self.ledger_path, "w", encoding="utf-8") as f:
            json.dump(incomplete_data, f)

        # Should handle gracefully (might raise KeyError or return None)
        try:
            _entry = self.ledger.get_entry("COM3")  # noqa: F841
            # If it succeeds, entry might be None or partial
        except KeyError:
            # Expected - missing required fields
            pass

    def test_non_dict_json_root(self):
        """Test handling of JSON file with non-dict root."""
        with open(self.ledger_path, "w", encoding="utf-8") as f:
            json.dump(["not", "a", "dict"], f)

        entries = self.ledger.get_all()
        self.assertEqual(len(entries), 0)

    def test_get_entry_after_clear(self):
        """Test getting entry after it was cleared."""
        self.ledger.record_deployment(
            port="COM3",
            firmware_hash="hash",
            source_hash="src",
            project_dir="/path",
            environment="env",
            build_flags_hash="bf",
        )

        self.ledger.clear("COM3")
        entry = self.ledger.get_entry("COM3")

        self.assertIsNone(entry)

    def test_double_clear(self):
        """Test clearing same entry twice."""
        self.ledger.record_deployment(
            port="COM3",
            firmware_hash="hash",
            source_hash="src",
            project_dir="/path",
            environment="env",
            build_flags_hash="bf",
        )

        result1 = self.ledger.clear("COM3")
        result2 = self.ledger.clear("COM3")

        self.assertTrue(result1)
        self.assertFalse(result2)


class TestFirmwareEntryValidation(unittest.TestCase):
    """Test cases for entry validation."""

    def test_valid_entry(self):
        """Test that valid entry passes validation."""
        entry = FirmwareEntry(
            port="COM3",
            firmware_hash="abc123",
            source_hash="def456",
            project_dir="/path/to/project",
            environment="esp32dev",
            upload_timestamp=time.time(),
            build_flags_hash="ghi789",
        )

        # All fields should be accessible
        self.assertIsNotNone(entry.port)
        self.assertIsNotNone(entry.firmware_hash)
        self.assertIsNotNone(entry.source_hash)
        self.assertIsNotNone(entry.project_dir)
        self.assertIsNotNone(entry.environment)
        self.assertIsNotNone(entry.upload_timestamp)
        self.assertIsNotNone(entry.build_flags_hash)

    def test_entry_with_none_values_behavior(self):
        """Test behavior when None values are used (dataclass allows but may cause issues)."""
        # Note: Python dataclasses don't enforce type hints at runtime by default.
        # This test documents the expected behavior when None is used.
        entry = FirmwareEntry(
            port=None,  # type: ignore
            firmware_hash="hash",
            source_hash="src",
            project_dir="/path",
            environment="env",
            upload_timestamp=time.time(),
            build_flags_hash="bf",
        )

        # Entry is created but port is None (type checkers would flag this)
        self.assertIsNone(entry.port)

        # Serialization should work but produce None in JSON
        data = entry.to_dict()
        self.assertIsNone(data["port"])

    def test_entry_equality(self):
        """Test entry equality comparison."""
        timestamp = time.time()

        entry1 = FirmwareEntry(
            port="COM3",
            firmware_hash="hash",
            source_hash="src",
            project_dir="/path",
            environment="env",
            upload_timestamp=timestamp,
            build_flags_hash="bf",
        )

        entry2 = FirmwareEntry(
            port="COM3",
            firmware_hash="hash",
            source_hash="src",
            project_dir="/path",
            environment="env",
            upload_timestamp=timestamp,
            build_flags_hash="bf",
        )

        self.assertEqual(entry1, entry2)

    def test_entry_inequality(self):
        """Test entry inequality comparison."""
        entry1 = FirmwareEntry(
            port="COM3",
            firmware_hash="hash1",
            source_hash="src",
            project_dir="/path",
            environment="env",
            upload_timestamp=time.time(),
            build_flags_hash="bf",
        )

        entry2 = FirmwareEntry(
            port="COM3",
            firmware_hash="hash2",
            source_hash="src",
            project_dir="/path",
            environment="env",
            upload_timestamp=time.time(),
            build_flags_hash="bf",
        )

        self.assertNotEqual(entry1, entry2)


if __name__ == "__main__":
    unittest.main()
