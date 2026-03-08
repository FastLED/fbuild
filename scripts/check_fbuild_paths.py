#!/usr/bin/env python3
"""Standalone script to enforce centralized .fbuild path resolution.

All .fbuild directory paths must go through src/fbuild/paths.py.
This script flags any direct ".fbuild" path construction outside that module.

Usage:
    python scripts/check_fbuild_paths.py
"""

import sys
from pathlib import Path

# Add project root to path for development (fbuild_lint is not distributed with package)
project_root = Path(__file__).parent.parent
if project_root not in sys.path:
    sys.path.insert(0, str(project_root))

from fbuild_lint.ruff_plugins.fbuild_path_checker import check_file


def main() -> int:
    """Run .fbuild path checker on all source files."""
    src_dir = Path(__file__).parent.parent / "src"

    # Only paths.py is allowed to construct .fbuild paths directly
    allowed_files = {"paths.py"}

    # Directories to skip
    excludes = {".fbuild", ".build", ".zap", "__pycache__"}

    total_errors = 0

    for file_path in src_dir.rglob("*.py"):
        # Skip excluded directories
        if any(excl in file_path.parts for excl in excludes):
            continue

        # Skip the canonical source of truth
        if file_path.name in allowed_files:
            continue

        errors = list(check_file(file_path))
        if errors:
            relative = file_path.relative_to(src_dir)
            print(f"{relative}:")
            for line_num, msg in errors:
                print(f"  Line {line_num}: {msg}")
            total_errors += len(errors)

    if total_errors > 0:
        print(f"\nTotal errors: {total_errors}")
        return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
