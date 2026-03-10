"""Staged installation utility for safe, atomic package installs.

Provides a context manager that installs to a temporary staging directory,
then atomically renames to the final location on success. If the install
is cancelled or fails, the staging directory is left as an orphan that
can be garbage-collected later by cleanup_stale_staging_dirs().
"""

import shutil
import time
import uuid
from contextlib import contextmanager
from pathlib import Path
from typing import Generator


@contextmanager
def staged_install(final_path: Path, staging_root: Path) -> Generator[Path, None, None]:
    """Install to a staging directory, then atomically rename to final_path on success.

    Creates a temporary directory under staging_root (e.g., platforms_dir/_installing_<uuid>).
    All install work should target this staging directory. On successful exit, the staging
    directory is renamed to final_path. On failure, it's left for later garbage collection.

    Args:
        final_path: The final install location (e.g., cache/platforms/<hash>/<version>/)
        staging_root: Parent directory for staging dirs (e.g., cache/platforms/)

    Yields:
        Path to the staging directory where install work should happen
    """
    staging_dir = staging_root / f"_installing_{uuid.uuid4().hex[:8]}"
    staging_dir.mkdir(parents=True, exist_ok=True)
    try:
        yield staging_dir

        # Success: commit staging → final
        if final_path.exists():
            shutil.rmtree(final_path)
        final_path.parent.mkdir(parents=True, exist_ok=True)
        staging_dir.rename(final_path)
    except KeyboardInterrupt as ke:
        from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

        handle_keyboard_interrupt_properly(ke)
        raise  # Never reached, but satisfies type checker
    except Exception:
        # Leave staging dir for garbage collection
        raise


def cleanup_stale_staging_dirs(staging_root: Path, max_age_seconds: int = 3600) -> None:
    """Remove leftover _installing_* staging dirs older than max_age_seconds.

    Only removes staging dirs whose mtime is older than the threshold to avoid
    interfering with concurrent installs that may still be in progress.

    Args:
        staging_root: Directory to scan for stale staging dirs
        max_age_seconds: Minimum age in seconds before a staging dir is considered stale
    """
    if not staging_root.exists():
        return
    cutoff = time.time() - max_age_seconds
    for entry in staging_root.iterdir():
        if entry.name.startswith("_installing_"):
            try:
                if entry.stat().st_mtime < cutoff:
                    print(f"WARNING: Stale install directory detected, removing {entry}")
                    shutil.rmtree(entry)
            except KeyboardInterrupt as ke:
                from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

                handle_keyboard_interrupt_properly(ke)
            except Exception:
                pass  # Best-effort cleanup
