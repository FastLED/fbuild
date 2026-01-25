#!/usr/bin/env python3
"""Standalone script to check orchestrator build() method signatures.

This script validates that all platform-specific orchestrators implement
the correct build() method signature with all required parameters and type annotations.

Usage:
    python scripts/check_orchestrator_signatures.py
"""

import ast
import sys
from pathlib import Path

# Add project root to path for development (fbuild_lint is not distributed with package)
project_root = Path(__file__).parent.parent
if project_root not in sys.path:
    sys.path.insert(0, str(project_root))

from fbuild_lint.ruff_plugins.orchestrator_signature_checker import OrchestratorSignatureChecker


def main() -> int:
    """Run orchestrator signature checker on all orchestrator files."""
    build_dir = Path(__file__).parent.parent / "src" / "fbuild" / "build"
    orchestrator_files = list(build_dir.glob("orchestrator_*.py"))

    if not orchestrator_files:
        print("No orchestrator files found")
        return 1

    total_errors = 0

    for file_path in sorted(orchestrator_files):
        print(f"Checking {file_path.name}...")

        with open(file_path) as f:
            tree = ast.parse(f.read(), filename=str(file_path))

        checker = OrchestratorSignatureChecker(tree)
        errors = list(checker.run())

        if errors:
            print(f"  Found {len(errors)} error(s):")
            for line, col, msg, _ in errors:
                print(f"    Line {line}: {msg}")
            total_errors += len(errors)
        else:
            print(f"  OK")

    print(f"\nTotal errors: {total_errors}")
    return 0 if total_errors == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
