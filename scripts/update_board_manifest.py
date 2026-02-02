#!/usr/bin/env python3
"""Update the board manifest file.

This script regenerates assets/boards/manifest.json to match the actual
board JSON files in assets/boards/json/.

Usage:
    python scripts/update_board_manifest.py
"""

import json
from pathlib import Path


def main() -> None:
    """Generate the board manifest from existing board files."""
    project_root = Path(__file__).parent.parent
    boards_dir = project_root / "assets" / "boards" / "json"
    manifest_path = project_root / "assets" / "boards" / "manifest.json"

    if not boards_dir.exists():
        print(f"Error: Boards directory not found: {boards_dir}")
        return

    # Get all board names from JSON files
    boards = sorted([f.stem for f in boards_dir.glob("*.json")])

    # Create manifest
    manifest = {
        "version": "1.0",
        "boards": boards,
    }

    # Write manifest
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"Updated manifest with {len(boards)} boards: {manifest_path}")


if __name__ == "__main__":
    main()
