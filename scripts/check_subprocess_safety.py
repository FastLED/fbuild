#!/usr/bin/env python3
"""Standalone script to check that subprocess calls use safe wrappers.

This script validates that all subprocess.run() and subprocess.Popen() calls
use the safe_run() and safe_popen() wrappers from subprocess_utils.py to
prevent console window issues on Windows.

Usage:
    python scripts/check_subprocess_safety.py
"""

import ast
import sys
from pathlib import Path

# Add project root to path for development (fbuild_lint is not distributed with package)
project_root = Path(__file__).parent.parent
if project_root not in sys.path:
    sys.path.insert(0, str(project_root))

from fbuild_lint.ruff_plugins.subprocess_safety_checker import SubprocessSafetyChecker


def main() -> int:
    """Run subprocess safety checker on all source files."""
    src_dir = Path(__file__).parent.parent / "src"

    # Exclusions matching current flake8 call
    excludes = {"subprocess_utils.py", "test_subprocess_utils.py"}

    total_errors = 0

    for file_path in src_dir.rglob("*.py"):
        if file_path.name in excludes:
            continue

        with open(file_path, encoding="utf-8") as f:
            tree = ast.parse(f.read(), filename=str(file_path))

        checker = SubprocessSafetyChecker(tree)
        errors = list(checker.run())

        if errors:
            print(f"{file_path.name}:")
            for line, col, msg, _ in errors:
                print(f"  Line {line}: {msg}")
            total_errors += len(errors)

    if total_errors > 0:
        print(f"\nTotal errors: {total_errors}")
        return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
