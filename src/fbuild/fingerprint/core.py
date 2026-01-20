#!/usr/bin/env python3
"""
Core Fingerprint Cache Implementation for fbuild

Provides hash-based fingerprint caching for efficient build change detection.
Adapted from FastLED CI fingerprint system.

Key features:
- SHA256 content hashing for reliable change detection
- File locking for concurrent access safety
- Version-based cache invalidation (rebuilds when fbuild version changes)
- Source file scanning for automatic cache key generation
"""

import hashlib
import json
import time
from pathlib import Path
from typing import Any

import fasteners

from fbuild import __version__ as FBUILD_VERSION


class HashFingerprintCache:
    """
    Hash-based fingerprint cache with file locking for concurrent access.

    Generates a single SHA256 hash from all provided file paths and their contents.
    Uses compare-and-swap operations to prevent race conditions when multiple processes
    are validating and updating the same cache.

    Features:
    - Version tracking: Cache is invalidated when fbuild version changes
    - Content hashing: SHA256 of file contents (not just mtime)
    - Concurrent-safe: File locking prevents race conditions
    - Pre-computed fingerprint: Immune to file changes during processing
    """

    def __init__(self, cache_dir: Path, name: str):
        """
        Initialize hash fingerprint cache.

        Args:
            cache_dir: Base cache directory (e.g., Path(".fbuild/build/env"))
            name: Cache name identifier (e.g., "source_files")
        """
        self.cache_dir = Path(cache_dir)
        self.name = name

        # Ensure cache directory exists
        self.cache_dir.mkdir(parents=True, exist_ok=True)

        # Cache and lock file paths
        self.cache_file = self.cache_dir / f"{name}_fingerprint.json"
        self.lock_file = str(self.cache_dir / f"{name}_fingerprint.lock")

        # Pending fingerprint data (stored before processing starts)
        self._pending_fingerprint: dict[str, Any] | None = None

    def _compute_files_hash(self, file_paths: list[Path]) -> str:
        """
        Compute combined SHA256 hash of all files.

        Args:
            file_paths: List of file paths to hash

        Returns:
            SHA256 hash as hexadecimal string
        """
        sha256 = hashlib.sha256()

        # Include fbuild version in hash (version changes invalidate cache)
        sha256.update(f"fbuild_version:{FBUILD_VERSION}".encode("utf-8"))

        # Sort paths for deterministic ordering
        sorted_paths = sorted(file_paths, key=str)

        for file_path in sorted_paths:
            # Include relative path in hash (so renames are detected)
            sha256.update(str(file_path).encode("utf-8"))

            # Hash file content
            if file_path.exists() and file_path.is_file():
                try:
                    with open(file_path, "rb") as f:
                        for chunk in iter(lambda: f.read(8192), b""):
                            sha256.update(chunk)
                except (OSError, IOError):
                    # If we can't read a file, include marker
                    sha256.update(b"UNREADABLE")
            else:
                # File doesn't exist
                sha256.update(b"MISSING")

        # Include file count to detect list changes
        sha256.update(f"file_count:{len(file_paths)}".encode("utf-8"))

        return sha256.hexdigest()

    def _read_cache_data(self) -> dict[str, Any] | None:
        """
        Read cache data from JSON file (should be called within lock context).

        Returns:
            Cache data dict or None if file doesn't exist or is corrupted
        """
        if not self.cache_file.exists():
            return None

        try:
            with open(self.cache_file, "r") as f:
                return json.load(f)
        except (json.JSONDecodeError, OSError):
            return None

    def _write_cache_data(self, data: dict[str, Any]) -> None:
        """
        Write cache data to JSON file (should be called within lock context).

        Args:
            data: Cache data to write
        """
        try:
            with open(self.cache_file, "w") as f:
                json.dump(data, f, indent=2)
        except OSError as e:
            raise RuntimeError(f"Failed to write cache file {self.cache_file}: {e}")

    def _store_pending_fingerprint(self, hash_value: str, timestamp: float, file_count: int) -> None:
        """Store pending fingerprint data in a temporary cache file."""
        pending_file = self.cache_file.with_suffix(".pending")
        pending_data = {
            "hash": hash_value,
            "timestamp": timestamp,
            "file_count": file_count,
            "version": FBUILD_VERSION,
            "name": self.name,
        }

        self._pending_fingerprint = pending_data

        try:
            with open(pending_file, "w") as f:
                json.dump(pending_data, f, indent=2)
        except OSError:
            # If we can't store pending data, that's okay - we'll fall back to force update
            pass

    def _load_pending_fingerprint(self) -> dict[str, Any] | None:
        """Load pending fingerprint data from temporary cache file."""
        pending_file = self.cache_file.with_suffix(".pending")
        if not pending_file.exists():
            return self._pending_fingerprint

        try:
            with open(pending_file, "r") as f:
                return json.load(f)
        except (json.JSONDecodeError, OSError):
            return self._pending_fingerprint

    def _clear_pending_fingerprint(self) -> None:
        """Remove the pending fingerprint file."""
        pending_file = self.cache_file.with_suffix(".pending")
        if pending_file.exists():
            try:
                pending_file.unlink()
            except OSError:
                pass
        self._pending_fingerprint = None

    def check_needs_update(self, file_paths: list[Path]) -> bool:
        """
        Check if files need to be processed and store fingerprint for later use.

        This is the safe pattern: compute fingerprint before processing, store it
        in the cache file temporarily, and use the stored fingerprint in mark_success()
        regardless of file changes during processing.

        Args:
            file_paths: List of file paths to check

        Returns:
            True if processing is needed, False if cache is valid
        """
        current_hash = self._compute_files_hash(file_paths)
        current_time = time.time()

        # Check if cache is valid
        lock = fasteners.InterProcessLock(self.lock_file)
        with lock:
            cache_data = self._read_cache_data()

            if cache_data is None:
                # No cache - store pending fingerprint and return needs update
                self._store_pending_fingerprint(current_hash, current_time, len(file_paths))
                return True

            # Check version first
            cached_version = cache_data.get("version")
            if cached_version != FBUILD_VERSION:
                # Version changed - invalidate cache
                self._store_pending_fingerprint(current_hash, current_time, len(file_paths))
                return True

            cached_hash = cache_data.get("hash")
            needs_update = cached_hash != current_hash

            if needs_update:
                # Store pending fingerprint for successful completion
                self._store_pending_fingerprint(current_hash, current_time, len(file_paths))

            return needs_update

    def mark_success(self) -> None:
        """
        Mark processing as successful using the pre-computed fingerprint.

        This method uses the fingerprint stored by check_needs_update(),
        making it immune to file changes during processing.
        """
        # Try to load pending fingerprint from file (cross-process safe)
        fingerprint_data = self._load_pending_fingerprint()

        if fingerprint_data is None:
            raise RuntimeError("mark_success() called without prior check_needs_update()")

        # Save using write lock
        lock = fasteners.InterProcessLock(self.lock_file)
        with lock:
            self._write_cache_data(fingerprint_data)

        # Clean up stored fingerprints
        self._clear_pending_fingerprint()

    def invalidate(self) -> None:
        """
        Invalidate the cache by removing the cache file.

        This forces the next validation to return True (needs update).
        """
        lock = fasteners.InterProcessLock(self.lock_file)
        with lock:
            if self.cache_file.exists():
                try:
                    self.cache_file.unlink()
                except OSError as e:
                    raise RuntimeError(f"Failed to invalidate cache file {self.cache_file}: {e}")

    def get_cache_info(self) -> dict[str, Any] | None:
        """
        Get information about the current cache state.

        Returns:
            Dict with cache information or None if no cache exists
        """
        lock = fasteners.InterProcessLock(self.lock_file)
        with lock:
            cache_data = self._read_cache_data()
            if cache_data is None:
                return None

            return {
                "hash": cache_data.get("hash"),
                "timestamp": cache_data.get("timestamp"),
                "version": cache_data.get("version"),
                "file_count": cache_data.get("file_count"),
                "cache_file": str(self.cache_file),
                "cache_exists": self.cache_file.exists(),
            }


class SourceFingerprintCache:
    """
    Source file fingerprint cache for build systems.

    Specialized cache that automatically scans a directory for source files
    and computes a combined hash. Designed for detecting when source files
    change and invalidating build caches accordingly.

    Supported file types: .ino, .cpp, .c, .h, .hpp, .cc, .cxx
    """

    # File extensions to scan for source files
    SOURCE_EXTENSIONS = {".ino", ".cpp", ".c", ".h", ".hpp", ".cc", ".cxx"}

    def __init__(self, cache_dir: Path, name: str = "source_files"):
        """
        Initialize source fingerprint cache.

        Args:
            cache_dir: Base cache directory
            name: Cache name identifier
        """
        self._hash_cache = HashFingerprintCache(cache_dir, name)

    @classmethod
    def scan_source_files(cls, source_dir: Path) -> list[Path]:
        """
        Scan directory for source files.

        Args:
            source_dir: Directory to scan

        Returns:
            Sorted list of source file paths
        """
        source_files: list[Path] = []

        if source_dir.exists():
            for ext in cls.SOURCE_EXTENSIONS:
                source_files.extend(source_dir.rglob(f"*{ext}"))

        # Sort for deterministic ordering
        return sorted(source_files)

    def check_needs_update(self, source_dir: Path) -> bool:
        """
        Check if source files have changed since last successful build.

        Args:
            source_dir: Directory containing source files

        Returns:
            True if rebuild is needed, False if cache is valid
        """
        source_files = self.scan_source_files(source_dir)
        return self._hash_cache.check_needs_update(source_files)

    def mark_success(self) -> None:
        """Mark build as successful, saving the fingerprint."""
        self._hash_cache.mark_success()

    def invalidate(self) -> None:
        """Invalidate the cache, forcing rebuild on next check."""
        self._hash_cache.invalidate()

    def get_cache_info(self) -> dict[str, Any] | None:
        """Get cache information."""
        return self._hash_cache.get_cache_info()

    @staticmethod
    def compute_hash_for_directory(source_dir: Path) -> str:
        """
        Compute hash for all source files in a directory.

        This is a static utility method for computing the hash without
        using the cache. Useful for comparing hashes directly.

        Args:
            source_dir: Directory to scan

        Returns:
            SHA256 hash as hexadecimal string
        """
        sha256 = hashlib.sha256()

        # Include fbuild version
        sha256.update(f"fbuild_version:{FBUILD_VERSION}".encode("utf-8"))

        # Find all source files
        source_files: list[Path] = []
        if source_dir.exists():
            for ext in SourceFingerprintCache.SOURCE_EXTENSIONS:
                source_files.extend(source_dir.rglob(f"*{ext}"))

        # Sort for deterministic ordering
        source_files = sorted(source_files)

        for file_path in source_files:
            # Include relative path in hash
            try:
                rel_path = file_path.relative_to(source_dir)
            except ValueError:
                rel_path = file_path
            sha256.update(str(rel_path).encode("utf-8"))

            # Hash file content
            try:
                with open(file_path, "rb") as f:
                    for chunk in iter(lambda: f.read(8192), b""):
                        sha256.update(chunk)
            except (OSError, IOError):
                sha256.update(b"UNREADABLE")

        return sha256.hexdigest()
