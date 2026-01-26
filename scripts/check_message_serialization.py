#!/usr/bin/env python3
"""Standalone script to check message serialization completeness.

This script validates that all dataclass message types properly serialize and
deserialize all their fields in to_dict() and from_dict() methods.

Usage:
    python scripts/check_message_serialization.py
"""

import ast
import sys
from pathlib import Path

# Add project root to path for development (fbuild_lint is not distributed with package)
project_root = Path(__file__).parent.parent
if project_root not in sys.path:
    sys.path.insert(0, str(project_root))

from fbuild_lint.ruff_plugins.message_serialization_checker import MessageSerializationChecker


def main() -> int:
    """Run message serialization checker on messages package."""
    messages_dir = Path(__file__).parent.parent / "src" / "fbuild" / "daemon" / "messages"

    if not messages_dir.exists():
        print(f"Messages directory not found: {messages_dir}")
        return 1

    # Collect all Python files in the messages package (except __pycache__)
    message_files = sorted(messages_dir.glob("*.py"))
    if not message_files:
        print(f"No Python files found in {messages_dir}")
        return 1

    print(f"Checking {len(message_files)} message files...")
    total_errors = 0

    for message_file in message_files:
        # Skip __init__.py and _base.py (base classes, not messages)
        if message_file.name in ("__init__.py", "_base.py"):
            continue

        print(f"  {message_file.name}...", end=" ")

        with open(message_file) as f:
            tree = ast.parse(f.read(), filename=str(message_file))

        checker = MessageSerializationChecker(tree)
        errors = list(checker.run())

        if errors:
            print(f"FAILED ({len(errors)} error(s))")
            for line, col, msg, _ in errors:
                print(f"    Line {line}: {msg}")
            total_errors += len(errors)
        else:
            print("OK")

    if total_errors > 0:
        print(f"\nTotal errors: {total_errors}")
        return 1
    else:
        print("\nAll message serialization is complete")
        return 0


if __name__ == "__main__":
    sys.exit(main())
