#!/usr/bin/env python3
"""Standalone script to check that try-except blocks properly handle KeyboardInterrupt.

This script validates that bare except clauses don't accidentally catch KeyboardInterrupt,
which should always propagate to allow users to cancel operations with Ctrl+C.

Usage:
    python scripts/check_keyboard_interrupt.py
"""

import ast
import sys
from pathlib import Path

# Add project root to path for development (fbuild_lint is not distributed with package)
project_root = Path(__file__).parent.parent
if project_root not in sys.path:
    sys.path.insert(0, str(project_root))

from fbuild_lint.ruff_plugins.keyboard_interrupt_checker import KeyboardInterruptChecker


def main() -> int:
    """Run KeyboardInterrupt checker on all source files."""
    src_dir = Path(__file__).parent.parent / "src"
    source_files = list(src_dir.rglob("*.py"))

    if not source_files:
        print("No source files found")
        return 1

    total_errors = 0

    for file_path in sorted(source_files):
        with open(file_path, encoding="utf-8") as f:
            tree = ast.parse(f.read(), filename=str(file_path))

        checker = KeyboardInterruptChecker(tree)
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
