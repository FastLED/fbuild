"""Centralized path resolution for fbuild.

All .fbuild directory paths are resolved through this module. Grep for
".fbuild" and you'll find this file — the single source of truth.

Directory layout:
    ~/.fbuild/
        dev/            (FBUILD_DEV_MODE=1)
            daemon/
            cache/
        prod/           (default)
            daemon/
            cache/

    <project>/.fbuild/  (always, regardless of mode)
        build/{env}/{profile}/

Environment variable overrides (highest priority):
    FBUILD_CACHE_DIR  — override cache root entirely
    FBUILD_BUILD_DIR  — override project build root entirely

All path constants (FBUILD_ROOT, DAEMON_DIR, CACHE_ROOT) are evaluated lazily
via __getattr__. This is necessary because cli.py sets FBUILD_DEV_MODE=1 AFTER
this module is first imported.
"""

import os
from pathlib import Path

# The ".fbuild" directory name — used in both global (~/.fbuild/) and
# project-local (<project>/.fbuild/) contexts. Defined here so that
# grepping for ".fbuild" leads to this module.
FBUILD_DIR = ".fbuild"


def is_dev_mode() -> bool:
    """Check if development mode is enabled."""
    return os.environ.get("FBUILD_DEV_MODE") == "1"


def get_fbuild_root() -> Path:
    """Get the mode-specific fbuild root directory.

    Returns:
        ~/.fbuild/dev  (dev mode)
        ~/.fbuild/prod (production)
    """
    mode = "dev" if is_dev_mode() else "prod"
    return Path.home() / FBUILD_DIR / mode


def get_other_fbuild_root() -> Path:
    """Get the fbuild root for the OTHER mode (cross-mode fallback).

    If current mode is dev, returns prod root, and vice versa.
    Used for cross-mode daemon discovery.
    """
    mode = "prod" if is_dev_mode() else "dev"
    return Path.home() / FBUILD_DIR / mode


def get_project_fbuild_dir(project_dir: Path) -> Path:
    """Get the .fbuild directory inside a project.

    This is the project-local directory for build artifacts, output files, etc.
    Always <project>/.fbuild/ regardless of dev/prod mode.
    """
    return project_dir / FBUILD_DIR


def get_project_build_root(project_dir: Path) -> Path:
    """Get the build root inside a project: <project>/.fbuild/build/."""
    return get_project_fbuild_dir(project_dir) / "build"


def get_daemon_dir() -> Path:
    """Get the daemon directory for the current mode."""
    return get_fbuild_root() / "daemon"


def get_cache_root() -> Path:
    """Get the cache root directory.

    Priority: FBUILD_CACHE_DIR > mode-based default.
    """
    cache_env = os.environ.get("FBUILD_CACHE_DIR")
    if cache_env:
        return Path(cache_env).resolve()
    return get_fbuild_root() / "cache"


def __getattr__(name: str) -> Path:
    """Lazy evaluation of path constants.

    Supports: FBUILD_ROOT, DAEMON_DIR, CACHE_ROOT.
    Re-evaluates on every access so FBUILD_DEV_MODE changes are respected.
    """
    if name == "FBUILD_ROOT":
        return get_fbuild_root()
    if name == "DAEMON_DIR":
        return get_daemon_dir()
    if name == "CACHE_ROOT":
        return get_cache_root()
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")
