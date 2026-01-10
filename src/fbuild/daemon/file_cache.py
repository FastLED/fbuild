"""File-level incremental compilation cache.

This module tracks source file changes for incremental compilation, allowing
the build system to skip recompilation of unchanged files.
"""

from __future__ import annotations

import hashlib
import json
import logging
import threading
from pathlib import Path
from typing import Optional

logger = logging.getLogger(__name__)


class FileCache:
    """Tracks source file changes for incremental compilation.

    Uses SHA256 hashing to detect file changes and maintains a persistent cache
    on disk. Thread-safe for concurrent use.
    """

    def __init__(self, cache_file: Path):
        """Initialize file cache.

        Args:
            cache_file: Path to cache file (JSON format)
        """
        self.cache_file = cache_file
        self.cache: dict[str, str] = {}
        self.lock = threading.Lock()
        self._load_cache()

    def _load_cache(self):
        """Load cache from disk."""
        if not self.cache_file.exists():
            logger.debug(f"Cache file not found: {self.cache_file}")
            return

        try:
            with open(self.cache_file, "r", encoding="utf-8") as f:
                self.cache = json.load(f)
            logger.info(f"Loaded cache with {len(self.cache)} entries from {self.cache_file}")
        except (json.JSONDecodeError, IOError) as e:
            logger.warning(f"Failed to load cache from {self.cache_file}: {e}")
            self.cache = {}

    def _save_cache(self):
        """Save cache to disk atomically.

        Uses atomic write pattern (temp file + rename) to prevent corruption.
        """
        try:
            # Ensure cache directory exists
            self.cache_file.parent.mkdir(parents=True, exist_ok=True)

            # Write to temporary file
            temp_file = self.cache_file.with_suffix(".tmp")
            with open(temp_file, "w", encoding="utf-8") as f:
                json.dump(self.cache, f, indent=2)

            # Atomic rename
            temp_file.replace(self.cache_file)

            logger.debug(f"Saved cache with {len(self.cache)} entries to {self.cache_file}")

        except IOError as e:
            logger.error(f"Failed to save cache to {self.cache_file}: {e}")

    def get_file_hash(self, file_path: Path) -> str:
        """Calculate SHA256 hash of file contents.

        Args:
            file_path: Path to file

        Returns:
            SHA256 hash as hex string

        Raises:
            FileNotFoundError: If file does not exist
            IOError: If file cannot be read
        """
        sha256 = hashlib.sha256()

        try:
            with open(file_path, "rb") as f:
                # Read in chunks for memory efficiency
                for chunk in iter(lambda: f.read(8192), b""):
                    sha256.update(chunk)

            return sha256.hexdigest()

        except FileNotFoundError:
            logger.error(f"File not found for hashing: {file_path}")
            raise
        except IOError as e:
            logger.error(f"Failed to read file for hashing: {file_path}: {e}")
            raise

    def has_changed(self, file_path: Path) -> bool:
        """Check if file has changed since last cache update.

        Args:
            file_path: Path to file

        Returns:
            True if file has changed or not in cache, False otherwise
        """
        if not file_path.exists():
            logger.warning(f"File does not exist: {file_path}")
            return True

        file_key = str(file_path.absolute())

        with self.lock:
            cached_hash = self.cache.get(file_key)

        # File not in cache - consider it changed
        if cached_hash is None:
            logger.debug(f"File not in cache: {file_path}")
            return True

        try:
            current_hash = self.get_file_hash(file_path)
            changed = current_hash != cached_hash

            if changed:
                logger.debug(f"File changed: {file_path}")
            else:
                logger.debug(f"File unchanged: {file_path}")

            return changed

        except (FileNotFoundError, IOError):
            # If we can't hash the file, assume it changed
            return True

    def update(self, file_path: Path):
        """Update cache with current file hash.

        Args:
            file_path: Path to file
        """
        if not file_path.exists():
            logger.warning(f"Cannot update cache for non-existent file: {file_path}")
            return

        try:
            file_key = str(file_path.absolute())
            current_hash = self.get_file_hash(file_path)

            with self.lock:
                self.cache[file_key] = current_hash
                self._save_cache()

            logger.debug(f"Updated cache for: {file_path}")

        except (FileNotFoundError, IOError) as e:
            logger.error(f"Failed to update cache for {file_path}: {e}")

    def update_batch(self, file_paths: list[Path]):
        """Update cache for multiple files efficiently.

        Args:
            file_paths: List of file paths to update
        """
        updated_count = 0

        for file_path in file_paths:
            if not file_path.exists():
                continue

            try:
                file_key = str(file_path.absolute())
                current_hash = self.get_file_hash(file_path)

                with self.lock:
                    self.cache[file_key] = current_hash
                    updated_count += 1

            except (FileNotFoundError, IOError) as e:
                logger.warning(f"Failed to update cache for {file_path}: {e}")

        # Save once after all updates
        with self.lock:
            self._save_cache()

        logger.info(f"Updated cache for {updated_count}/{len(file_paths)} files")

    def needs_recompilation(self, source_path: Path, object_path: Path) -> bool:
        """Check if source file needs recompilation.

        A file needs recompilation if:
        1. Object file doesn't exist
        2. Source file has changed (cache check)
        3. Object file is older than source file (mtime check)

        Args:
            source_path: Path to source file (.c, .cpp, etc.)
            object_path: Path to object file (.o)

        Returns:
            True if recompilation needed, False otherwise
        """
        # Object doesn't exist - must compile
        if not object_path.exists():
            logger.debug(f"Object file missing: {object_path} - recompilation needed")
            return True

        # Source changed - must recompile
        if self.has_changed(source_path):
            logger.debug(f"Source file changed: {source_path} - recompilation needed")
            return True

        # Object older than source - must recompile
        try:
            source_mtime = source_path.stat().st_mtime
            object_mtime = object_path.stat().st_mtime

            if object_mtime < source_mtime:
                logger.debug(f"Object file older than source: {object_path} - recompilation needed")
                return True

        except OSError as e:
            logger.warning(f"Failed to check file times: {e} - assuming recompilation needed")
            return True

        # No recompilation needed
        logger.debug(f"Skipping unchanged file: {source_path}")
        return False

    def invalidate(self, file_path: Path):
        """Remove file from cache, forcing recompilation on next build.

        Args:
            file_path: Path to file
        """
        file_key = str(file_path.absolute())

        with self.lock:
            if file_key in self.cache:
                del self.cache[file_key]
                self._save_cache()
                logger.debug(f"Invalidated cache entry: {file_path}")

    def clear(self):
        """Clear entire cache."""
        with self.lock:
            self.cache.clear()
            self._save_cache()
            logger.info("Cache cleared")

    def get_statistics(self) -> dict[str, int]:
        """Get cache statistics.

        Returns:
            Dictionary with cache statistics
        """
        with self.lock:
            return {
                "total_entries": len(self.cache),
            }


# Global file cache instance (initialized by daemon)
_file_cache: Optional[FileCache] = None


def get_file_cache() -> FileCache:
    """Get global file cache instance.

    Returns:
        Global FileCache instance

    Raises:
        RuntimeError: If file cache not initialized
    """
    global _file_cache
    if _file_cache is None:
        raise RuntimeError("FileCache not initialized. Call init_file_cache() first.")
    return _file_cache


def init_file_cache(cache_file: Path) -> FileCache:
    """Initialize global file cache.

    Args:
        cache_file: Path to cache file

    Returns:
        Initialized FileCache instance
    """
    global _file_cache
    _file_cache = FileCache(cache_file=cache_file)
    logger.info(f"FileCache initialized with cache file: {cache_file}")
    return _file_cache
