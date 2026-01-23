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

from fbuild_lint.ruff_plugins.message_serialization_checker import MessageSerializationChecker


def main() -> int:
    """Run message serialization checker on messages.py."""
    messages_file = Path(__file__).parent.parent / "src" / "fbuild" / "daemon" / "messages.py"

    if not messages_file.exists():
        print(f"Messages file not found: {messages_file}")
        return 1

    print(f"Checking {messages_file.name}...")

    with open(messages_file) as f:
        tree = ast.parse(f.read(), filename=str(messages_file))

    checker = MessageSerializationChecker(tree)
    errors = list(checker.run())

    if errors:
        print(f"Found {len(errors)} error(s):")
        for line, col, msg, _ in errors:
            print(f"  Line {line}: {msg}")
        return 1
    else:
        print("OK - all message serialization is complete")
        return 0


if __name__ == "__main__":
    sys.exit(main())
