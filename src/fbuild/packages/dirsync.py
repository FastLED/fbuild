"""Smart directory synchronization (rsync-like).

Syncs a source directory to a destination directory, only copying files
that have actually changed. This avoids modifying timestamps on unchanged
files, which prevents unnecessary recompilation.

Algorithm for each file:
1. If file exists only in src: copy it to dst
2. If file exists only in dst: delete it
3. If file exists in both:
   a. Compare mtime - if identical, skip (file unchanged)
   b. If mtime differs, compare content hash (SHA-256)
   c. If hash differs: copy src -> dst (real change)
   d. If hash matches: touch dst to match src mtime (no real change)
"""

import hashlib
import logging
import os
import shutil
from pathlib import Path

logger = logging.getLogger(__name__)

# Directories to always skip during sync
_DEFAULT_IGNORE = frozenset(
    {
        ".fbuild",
        ".pio",
        ".git",
        ".venv",
        "__pycache__",
        ".pytest_cache",
        "node_modules",
        ".cache",
        "build",
        ".build",
        ".vscode",
        ".idea",
    }
)


def _file_hash(path: Path) -> str:
    """Compute SHA-256 hash of a file's contents."""
    sha = hashlib.sha256()
    with open(path, "rb") as f:
        while True:
            chunk = f.read(65536)
            if not chunk:
                break
            sha.update(chunk)
    return sha.hexdigest()


def _collect_relative_paths(root: Path, ignore_dirs: frozenset[str]) -> set[str]:
    """Walk a directory tree and collect all file paths relative to root."""
    result: set[str] = set()
    for dirpath, dirnames, filenames in os.walk(root):
        # Filter out ignored directories in-place (prevents os.walk from descending)
        dirnames[:] = [d for d in dirnames if d not in ignore_dirs]
        for fname in filenames:
            abs_path = os.path.join(dirpath, fname)
            rel = os.path.relpath(abs_path, root)
            # Normalize to forward slashes for consistency
            result.add(rel.replace("\\", "/"))
    return result


def sync_directory(
    src: Path,
    dst: Path,
    ignore_dirs: frozenset[str] | None = None,
) -> bool:
    """Sync src directory to dst, only copying files that actually changed.

    Args:
        src: Source directory
        dst: Destination directory
        ignore_dirs: Set of directory names to skip. If None, uses default set.
        on_ignore: Optional callback(directory_path, name) for ignored dirs.

    Returns:
        True if any files were copied or deleted (real changes occurred),
        False if dst was already in sync with src.
    """
    if ignore_dirs is None:
        ignore_dirs = _DEFAULT_IGNORE

    changed = False

    # Collect all relative paths in both trees
    src_files = _collect_relative_paths(src, ignore_dirs)
    dst_files = _collect_relative_paths(dst, ignore_dirs) if dst.exists() else set()

    # Ensure dst exists
    dst.mkdir(parents=True, exist_ok=True)

    # Files only in dst -> delete
    for rel in sorted(dst_files - src_files):
        dst_path = dst / rel
        if dst_path.exists():
            logger.debug(f"[dirsync] Deleting removed file: {rel}")
            dst_path.unlink()
            changed = True

    # Files in src (new or potentially updated)
    for rel in sorted(src_files):
        src_path = src / rel
        dst_path = dst / rel

        if not dst_path.exists():
            # New file -> copy
            logger.debug(f"[dirsync] Copying new file: {rel}")
            dst_path.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(str(src_path), str(dst_path))
            changed = True
            continue

        # Both exist -> compare
        src_stat = src_path.stat()
        dst_stat = dst_path.stat()

        # Compare size first (fast rejection)
        if src_stat.st_size != dst_stat.st_size:
            logger.debug(f"[dirsync] Size changed, copying: {rel}")
            shutil.copy2(str(src_path), str(dst_path))
            changed = True
            continue

        # Compare mtime (with small tolerance for filesystem precision)
        if abs(src_stat.st_mtime - dst_stat.st_mtime) < 0.01:
            # Same mtime -> skip
            continue

        # Mtime differs -> compare content hash
        src_hash = _file_hash(src_path)
        dst_hash = _file_hash(dst_path)

        if src_hash != dst_hash:
            # Content actually changed -> copy
            logger.debug(f"[dirsync] Content changed, copying: {rel}")
            shutil.copy2(str(src_path), str(dst_path))
            changed = True
        else:
            # Same content, different mtime -> update dst mtime to match src
            # This prevents future hash comparisons
            logger.debug(f"[dirsync] Same content, syncing mtime: {rel}")
            os.utime(str(dst_path), (src_stat.st_atime, src_stat.st_mtime))

    # Clean up empty directories in dst
    if dst.exists():
        for dirpath, dirnames, filenames in os.walk(str(dst), topdown=False):
            dirnames[:] = [d for d in dirnames if d not in ignore_dirs]
            if not filenames and not dirnames:
                dir_p = Path(dirpath)
                if dir_p != dst:
                    try:
                        dir_p.rmdir()
                    except OSError:
                        pass

    return changed
