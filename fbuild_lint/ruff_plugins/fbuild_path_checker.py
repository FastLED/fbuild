"""Lint plugin to enforce centralized .fbuild path resolution.

All .fbuild directory paths must be resolved through fbuild.paths — the single
source of truth. Direct construction of ".fbuild" paths via Path division (/)
is forbidden outside of that module.

Error Codes:
    FBP001: Direct ".fbuild" path construction — use fbuild.paths instead

Usage:
    python scripts/check_fbuild_paths.py
"""

import re
from pathlib import Path
from typing import Generator, Tuple

# Pattern: ".fbuild" used in path division (/ ".fbuild" or ".fbuild" /)
# This catches path construction like: project_dir / ".fbuild" / "build"
# but NOT directory name exclusion like: {".fbuild", ".pio", ".git"}
_PATH_DIVISION_PATTERN = re.compile(
    r'/\s*["\']\.fbuild["\']'  # something / ".fbuild"
    r"|"
    r'["\']\.fbuild["\']\s*/'  # ".fbuild" / something
)

ERROR_MSG = 'FBP001 Direct ".fbuild" path construction — use fbuild.paths instead'


def check_file(file_path: Path) -> Generator[Tuple[int, str], None, None]:
    """Check a file for direct .fbuild path construction.

    Args:
        file_path: Path to the Python file to check

    Yields:
        Tuple of (line_number, error_message)
    """
    try:
        lines = file_path.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeDecodeError):
        return

    in_docstring = False
    docstring_delimiter = None

    for line_num, line in enumerate(lines, start=1):
        stripped = line.strip()

        # Track docstrings (triple-quoted strings)
        if not in_docstring:
            if stripped.startswith('"""') or stripped.startswith("'''"):
                delimiter = stripped[:3]
                # Single-line docstring
                if stripped.count(delimiter) >= 2:
                    continue
                in_docstring = True
                docstring_delimiter = delimiter
                continue
        else:
            if docstring_delimiter and docstring_delimiter in stripped:
                in_docstring = False
                docstring_delimiter = None
            continue

        # Skip comments
        if stripped.startswith("#"):
            continue

        # Flag lines with ".fbuild" used in path division
        if _PATH_DIVISION_PATTERN.search(line):
            yield (line_num, ERROR_MSG)
