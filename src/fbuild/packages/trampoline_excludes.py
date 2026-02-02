"""Trampoline Exclusion Patterns.

These patterns are excluded from the header trampoline system because they
use GCC's #include_next directive, which requires the header's directory
to be present in the include search path.

When a header uses #include_next, the compiler searches for the next
occurrence of that header AFTER the current file's directory in the -I list.
If we trampoline these headers, the original directory is no longer in the
-I list (we use absolute paths), breaking #include_next resolution.

Example of the problem:
    1. Compiler finds `~/.fbuild/cache/trampolines/esp32c6/stdio.h`
    2. Trampoline redirects via `#include "absolute/path/to/newlib/platform_include/stdio.h"`
    3. Original file uses `#include_next <stdio.h>` to chain to toolchain's stdio.h
    4. #include_next searches after the current file's directory in the -I list
    5. But newlib/platform_include/ is NOT in the -I list (accessed via absolute path)
    6. GCC cannot resolve the next header correctly
"""

from pathlib import Path
from typing import List

# Headers that use #include_next to chain to system headers
# All patterns use forward slashes - paths are normalized before matching
INCLUDE_NEXT_PATTERNS: list[str] = [
    "newlib/platform_include",
]


def get_exclude_patterns() -> list[str]:
    """Get list of path patterns to exclude from trampolining.

    Returns a copy to prevent accidental modification.
    """
    return list(INCLUDE_NEXT_PATTERNS)


def should_exclude_path(path: Path) -> bool:
    """Check if a path should be excluded from trampolining.

    Normalizes the path to forward slashes before checking patterns.

    Args:
        path: Path to check

    Returns:
        True if the path matches an exclusion pattern
    """
    # Normalize to forward slashes for consistent matching
    path_str = str(path).replace("\\", "/")

    for pattern in INCLUDE_NEXT_PATTERNS:
        if pattern in path_str:
            return True
    return False


def filter_paths(include_paths: List[Path]) -> tuple[List[Path], List[Path]]:
    """Filter include paths into trampolined and excluded lists.

    Args:
        include_paths: List of include paths to filter

    Returns:
        Tuple of (paths_to_trampoline, excluded_paths)
    """
    filtered = []
    excluded = []

    for path in include_paths:
        if should_exclude_path(path):
            excluded.append(path)
        else:
            filtered.append(path)

    return filtered, excluded
