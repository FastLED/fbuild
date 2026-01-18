"""Package fingerprinting for artifact validation and persistence.

This module provides fingerprinting capabilities for downloaded packages,
enabling validation of installation state and detection of corruption or
incomplete installations.
"""

import hashlib
import json
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Dict, List, Optional


@dataclass
class PackageFingerprint:
    """Fingerprint for validating package installation state.

    A fingerprint captures the identity and state of a downloaded and
    extracted package, enabling:
    - Verification that an installation matches expected state
    - Detection of corruption or incomplete extraction
    - Cache validation without re-downloading
    """

    url: str
    version: str
    url_hash: str  # SHA256[:16] of URL (for cache key)
    content_hash: str  # SHA256 of downloaded archive
    extracted_files: List[str] = field(default_factory=list)  # Key files to verify
    install_timestamp: float = field(default_factory=time.time)
    file_count: int = 0
    total_size: int = 0
    metadata: Dict[str, Any] = field(default_factory=dict)

    FINGERPRINT_FILENAME = ".fbuild_fingerprint.json"

    @staticmethod
    def hash_url(url: str) -> str:
        """Generate a SHA256 hash of a URL for cache directory naming.

        Args:
            url: The base URL to hash

        Returns:
            First 16 characters of SHA256 hash (sufficient for uniqueness)
        """
        return hashlib.sha256(url.encode("utf-8")).hexdigest()[:16]

    @staticmethod
    def hash_file(file_path: Path, chunk_size: int = 8192) -> str:
        """Generate SHA256 hash of a file.

        Args:
            file_path: Path to file to hash
            chunk_size: Size of chunks for reading

        Returns:
            SHA256 hex digest
        """
        sha256 = hashlib.sha256()
        with open(file_path, "rb") as f:
            for chunk in iter(lambda: f.read(chunk_size), b""):
                sha256.update(chunk)
        return sha256.hexdigest()

    @classmethod
    def from_archive(
        cls,
        url: str,
        version: str,
        archive_path: Path,
        extracted_dir: Optional[Path] = None,
        key_files: Optional[List[str]] = None,
        metadata: Optional[Dict[str, Any]] = None,
    ) -> "PackageFingerprint":
        """Create fingerprint from a downloaded archive.

        Args:
            url: Original download URL
            version: Package version string
            archive_path: Path to downloaded archive
            extracted_dir: Optional path to extracted directory for file enumeration
            key_files: Optional list of key file paths to track (relative to extracted_dir)
            metadata: Optional additional metadata to store

        Returns:
            PackageFingerprint instance
        """
        url_hash = cls.hash_url(url)
        content_hash = cls.hash_file(archive_path)

        extracted_files: List[str] = []
        file_count = 0
        total_size = 0

        if extracted_dir and extracted_dir.exists():
            # Enumerate files in extracted directory
            for file_path in extracted_dir.rglob("*"):
                if file_path.is_file():
                    file_count += 1
                    total_size += file_path.stat().st_size

            # Track key files for quick validation
            if key_files:
                for key_file in key_files:
                    full_path = extracted_dir / key_file
                    if full_path.exists():
                        extracted_files.append(key_file)

        return cls(
            url=url,
            version=version,
            url_hash=url_hash,
            content_hash=content_hash,
            extracted_files=extracted_files,
            install_timestamp=time.time(),
            file_count=file_count,
            total_size=total_size,
            metadata=metadata or {},
        )

    def to_dict(self) -> Dict[str, Any]:
        """Convert fingerprint to dictionary for JSON serialization."""
        return {
            "url": self.url,
            "version": self.version,
            "url_hash": self.url_hash,
            "content_hash": self.content_hash,
            "extracted_files": self.extracted_files,
            "install_timestamp": self.install_timestamp,
            "file_count": self.file_count,
            "total_size": self.total_size,
            "metadata": self.metadata,
        }

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "PackageFingerprint":
        """Create fingerprint from dictionary."""
        return cls(
            url=data["url"],
            version=data["version"],
            url_hash=data["url_hash"],
            content_hash=data["content_hash"],
            extracted_files=data.get("extracted_files", []),
            install_timestamp=data.get("install_timestamp", 0),
            file_count=data.get("file_count", 0),
            total_size=data.get("total_size", 0),
            metadata=data.get("metadata", {}),
        )

    def save(self, directory: Path) -> Path:
        """Save fingerprint to directory.

        Args:
            directory: Directory to save fingerprint file

        Returns:
            Path to saved fingerprint file
        """
        directory.mkdir(parents=True, exist_ok=True)
        fingerprint_path = directory / self.FINGERPRINT_FILENAME

        with open(fingerprint_path, "w", encoding="utf-8") as f:
            json.dump(self.to_dict(), f, indent=2)

        return fingerprint_path

    @classmethod
    def load(cls, directory: Path) -> Optional["PackageFingerprint"]:
        """Load fingerprint from directory.

        Args:
            directory: Directory containing fingerprint file

        Returns:
            PackageFingerprint instance or None if not found/invalid
        """
        fingerprint_path = directory / cls.FINGERPRINT_FILENAME

        if not fingerprint_path.exists():
            return None

        try:
            with open(fingerprint_path, "r", encoding="utf-8") as f:
                data = json.load(f)
            return cls.from_dict(data)
        except (json.JSONDecodeError, KeyError, TypeError):
            return None

    def matches(self, other: "PackageFingerprint") -> bool:
        """Check if fingerprints match (same content).

        Args:
            other: Another fingerprint to compare

        Returns:
            True if fingerprints represent the same package installation
        """
        return self.url == other.url and self.version == other.version and self.content_hash == other.content_hash

    def validate_installation(self, directory: Path) -> tuple[bool, str]:
        """Validate that installation matches fingerprint.

        Args:
            directory: Directory containing extracted package

        Returns:
            Tuple of (is_valid, reason)
        """
        if not directory.exists():
            return False, "Directory does not exist"

        # Check key files exist
        for key_file in self.extracted_files:
            full_path = directory / key_file
            if not full_path.exists():
                return False, f"Missing key file: {key_file}"

        # Quick file count check (allows for some variance due to temp files)
        if self.file_count > 0:
            actual_count = sum(1 for _ in directory.rglob("*") if _.is_file())
            # Allow 10% variance in file count
            if actual_count < self.file_count * 0.9:
                return False, f"File count mismatch: expected ~{self.file_count}, found {actual_count}"

        return True, "Installation valid"


class FingerprintRegistry:
    """Registry for managing package fingerprints across a cache.

    Provides a centralized way to track all installed packages and
    their fingerprints.
    """

    REGISTRY_FILENAME = ".fbuild_package_registry.json"

    def __init__(self, cache_root: Path):
        """Initialize fingerprint registry.

        Args:
            cache_root: Root directory of the cache
        """
        self.cache_root = cache_root
        self.registry_path = cache_root / self.REGISTRY_FILENAME
        self._registry: Dict[str, Dict[str, Any]] = {}
        self._load()

    def _load(self) -> None:
        """Load registry from disk."""
        if self.registry_path.exists():
            try:
                with open(self.registry_path, "r", encoding="utf-8") as f:
                    self._registry = json.load(f)
            except (json.JSONDecodeError, IOError):
                self._registry = {}

    def _save(self) -> None:
        """Save registry to disk."""
        self.cache_root.mkdir(parents=True, exist_ok=True)
        with open(self.registry_path, "w", encoding="utf-8") as f:
            json.dump(self._registry, f, indent=2)

    def register(self, fingerprint: PackageFingerprint, install_path: Path) -> None:
        """Register a package installation.

        Args:
            fingerprint: Package fingerprint
            install_path: Path where package is installed
        """
        key = f"{fingerprint.url_hash}:{fingerprint.version}"
        self._registry[key] = {
            "fingerprint": fingerprint.to_dict(),
            "install_path": str(install_path),
            "registered_at": time.time(),
        }
        self._save()

    def get_fingerprint(self, url: str, version: str) -> Optional[PackageFingerprint]:
        """Get fingerprint for a package.

        Args:
            url: Package URL
            version: Package version

        Returns:
            PackageFingerprint or None if not registered
        """
        url_hash = PackageFingerprint.hash_url(url)
        key = f"{url_hash}:{version}"

        if key not in self._registry:
            return None

        try:
            return PackageFingerprint.from_dict(self._registry[key]["fingerprint"])
        except (KeyError, TypeError):
            return None

    def get_install_path(self, url: str, version: str) -> Optional[Path]:
        """Get installation path for a package.

        Args:
            url: Package URL
            version: Package version

        Returns:
            Installation path or None if not registered
        """
        url_hash = PackageFingerprint.hash_url(url)
        key = f"{url_hash}:{version}"

        if key not in self._registry:
            return None

        try:
            return Path(self._registry[key]["install_path"])
        except (KeyError, TypeError):
            return None

    def is_installed(self, url: str, version: str) -> bool:
        """Check if a package is installed and valid.

        Args:
            url: Package URL
            version: Package version

        Returns:
            True if package is installed and fingerprint is valid
        """
        fingerprint = self.get_fingerprint(url, version)
        if fingerprint is None:
            return False

        install_path = self.get_install_path(url, version)
        if install_path is None or not install_path.exists():
            return False

        is_valid, _ = fingerprint.validate_installation(install_path)
        return is_valid

    def unregister(self, url: str, version: str) -> bool:
        """Unregister a package.

        Args:
            url: Package URL
            version: Package version

        Returns:
            True if package was registered and removed
        """
        url_hash = PackageFingerprint.hash_url(url)
        key = f"{url_hash}:{version}"

        if key in self._registry:
            del self._registry[key]
            self._save()
            return True
        return False

    def list_packages(self) -> List[Dict[str, Any]]:
        """List all registered packages.

        Returns:
            List of package information dictionaries
        """
        packages = []
        for _key, entry in self._registry.items():
            try:
                fingerprint = PackageFingerprint.from_dict(entry["fingerprint"])
                install_path = Path(entry["install_path"])
                is_valid, reason = fingerprint.validate_installation(install_path)

                packages.append(
                    {
                        "url": fingerprint.url,
                        "version": fingerprint.version,
                        "url_hash": fingerprint.url_hash,
                        "install_path": str(install_path),
                        "is_valid": is_valid,
                        "validation_reason": reason,
                        "file_count": fingerprint.file_count,
                        "total_size": fingerprint.total_size,
                        "install_timestamp": fingerprint.install_timestamp,
                    }
                )
            except (KeyError, TypeError):
                continue

        return packages

    def cleanup_invalid(self) -> int:
        """Remove entries for invalid/missing installations.

        Returns:
            Number of entries removed
        """
        keys_to_remove = []

        for key, entry in self._registry.items():
            try:
                fingerprint = PackageFingerprint.from_dict(entry["fingerprint"])
                install_path = Path(entry["install_path"])

                if not install_path.exists():
                    keys_to_remove.append(key)
                    continue

                is_valid, _ = fingerprint.validate_installation(install_path)
                if not is_valid:
                    keys_to_remove.append(key)
            except (KeyError, TypeError):
                keys_to_remove.append(key)

        for key in keys_to_remove:
            del self._registry[key]

        if keys_to_remove:
            self._save()

        return len(keys_to_remove)
