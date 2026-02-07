#!/usr/bin/env python3
"""Standalone script to check for improper sys.path.insert() usage.

This script validates that sys.path.insert() calls are only used in approved contexts
(test files) to prevent fragile import hacks in production code.

Usage:
    python scripts/check_sys_path.py
"""

import ast
import sys
from pathlib import Path

# Add project root to path for development (fbuild_lint is not distributed with package)
project_root = Path(__file__).parent.parent
if project_root not in sys.path:
    sys.path.insert(0, str(project_root))

from fbuild_lint.ruff_plugins.sys_path_checker import SysPathChecker


def main() -> int:
    """Run sys.path checker on source and scripts (tests are allowed to use sys.path.insert)."""
    base_dir = Path(__file__).parent.parent

    # Check src and scripts only (tests are allowed to use sys.path.insert for fbuild_lint imports)
    search_dirs = [
        base_dir / "src",
        base_dir / "scripts",
    ]

    # Exclusions matching current flake8 call
    excludes = {".fbuild", ".build", ".zap"}

    # Also skip the standalone checker scripts themselves (they need sys.path for fbuild_lint import)
    checker_scripts = {"check_keyboard_interrupt.py", "check_sys_path.py", "check_subprocess_safety.py",
                      "check_orchestrator_signatures.py", "check_message_serialization.py",
                      "demo_parallel_install.py"}

    total_errors = 0

    for search_dir in search_dirs:
        if not search_dir.exists():
            continue

        for file_path in search_dir.rglob("*.py"):
            # Skip excluded directories
            if any(excl in file_path.parts for excl in excludes):
                continue

            # Skip checker scripts (they need sys.path.insert for fbuild_lint)
            if file_path.name in checker_scripts:
                continue

            with open(file_path, encoding="utf-8") as f:
                tree = ast.parse(f.read(), filename=str(file_path))

            checker = SysPathChecker(tree)
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
