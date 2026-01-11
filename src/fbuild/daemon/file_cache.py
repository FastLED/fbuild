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
        logger.info(f"FileCache initialized with {len(self.cache)} cached entries")

    def _load_cache(self):
        """Load cache from disk."""
        if not self.cache_file.exists():
            return

        try:
            with open(self.cache_file, "r", encoding="utf-8") as f:
                data = json.load(f)
            logger.debug(f"Parsed {len(data)} cache entries from JSON")
            self.cache = data
            logger.info(f"Loaded cache with {len(self.cache)} entries from {self.cache_file}")
            if len(self.cache) > 0:
                logger.debug(f"Sample cache keys: {list(self.cache.keys())[:3]}")
        except json.JSONDecodeError as e:
            logger.warning(f"Failed to parse cache JSON from {self.cache_file}: {e}")
            self.cache = {}
        except IOError as e:
            logger.warning(f"Failed to read cache file {self.cache_file}: {e}")
            self.cache = {}

    def _save_cache(self):
        """Save cache to disk atomically.

        Uses atomic write pattern (temp file + rename) to prevent corruption.
        """
        logger.debug(f"Cache entries to save: {len(self.cache)}")

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
            logger.debug(f"Cache save error details: {type(e).__name__}: {e}")

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
        bytes_read = 0

        try:
            with open(file_path, "rb") as f:
                # Read in chunks for memory efficiency
                for chunk in iter(lambda: f.read(8192), b""):
                    sha256.update(chunk)
                    bytes_read += len(chunk)

            hash_value = sha256.hexdigest()
            logger.debug(f"File hash computed: {hash_value[:16]}... (read {bytes_read} bytes)")
            return hash_value

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
            return True

        try:
            current_hash = self.get_file_hash(file_path)
            changed = current_hash != cached_hash

            if changed:
                logger.debug(f"File changed: {file_path} (cached: {cached_hash[:16]}..., current: {current_hash[:16]}...)")
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
                was_cached = file_key in self.cache
                self.cache[file_key] = current_hash
                cache_size = len(self.cache)

            logger.debug(f"Cache entry {'updated' if was_cached else 'added'}: {file_path} (total entries: {cache_size})")

            with self.lock:
                self._save_cache()

        except (FileNotFoundError, IOError) as e:
            logger.error(f"Failed to update cache for {file_path}: {e}")

    def update_batch(self, file_paths: list[Path]):
        """Update cache for multiple files efficiently.

        Args:
            file_paths: List of file paths to update
        """
        logger.info(f"Batch cache update starting: {len(file_paths)} files")
        updated_count = 0
        skipped_count = 0
        failed_count = 0

        for file_path in file_paths:
            if not file_path.exists():
                skipped_count += 1
                continue

            try:
                file_key = str(file_path.absolute())
                current_hash = self.get_file_hash(file_path)

                with self.lock:
                    self.cache[file_key] = current_hash
                    updated_count += 1

            except (FileNotFoundError, IOError) as e:
                logger.warning(f"Failed to update cache for {file_path}: {e}")
                failed_count += 1

        # Save once after all updates
        logger.debug(f"Saving batch cache update to disk ({updated_count} files updated)")
        with self.lock:
            self._save_cache()

        logger.info(f"Batch cache update complete: {updated_count}/{len(file_paths)} files updated, {skipped_count} skipped, {failed_count} failed")
        logger.debug(f"Total cache entries after batch update: {len(self.cache)}")

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
            logger.debug("Reason: object file does not exist")
            return True

        # Source changed - must recompile
        if self.has_changed(source_path):
            logger.debug("Reason: source file hash differs from cache")
            return True

        # Object older than source - must recompile
        try:
            source_mtime = source_path.stat().st_mtime
            object_mtime = object_path.stat().st_mtime

            if object_mtime < source_mtime:
                logger.debug(f"Reason: object mtime ({object_mtime}) < source mtime ({source_mtime})")
                return True

        except OSError as e:
            logger.warning(f"Failed to check file times: {e} - assuming recompilation needed")
            logger.debug(f"Reason: stat() failed with {type(e).__name__}")
            return True

        # No recompilation needed
        logger.debug(f"Skipping unchanged file: {source_path} (all checks passed)")
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
                logger.info(f"Invalidated cache entry: {file_path} (total entries: {len(self.cache)})")
            else:
                logger.debug(f"Cache entry not found: {file_path}")

    def clear(self):
        """Clear entire cache."""
        logger.info(f"Clearing entire cache (current entries: {len(self.cache)})")
        with self.lock:
            old_size = len(self.cache)
            self.cache.clear()
            self._save_cache()
            logger.info(f"Cache cleared: removed {old_size} entries")

    def get_statistics(self) -> dict[str, int]:
        """Get cache statistics.

        Returns:
            Dictionary with cache statistics
        """
        with self.lock:
            stats = {
                "total_entries": len(self.cache),
            }
        return stats


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
