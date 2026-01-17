#!/usr/bin/env python3
"""Clean up corrupted build state with long-path directories."""

import subprocess
import sys
from pathlib import Path


def remove_with_robocopy(path: Path) -> bool:
    """Remove directory using robocopy mirror technique (Windows-specific)."""
    if not path.exists():
        print(f"Path does not exist: {path}")
        return True

    # Create empty temporary directory
    project_dir = Path(__file__).parent
    empty_dir = project_dir / ".empty_temp"
    empty_dir.mkdir(exist_ok=True)

    try:
        print(f"Using robocopy to mirror empty directory over: {path}")
        # robocopy /MIR mirrors source to destination (makes destination empty)
        # /NFL /NDL /NJH /NJS /nc /ns /np = suppress output
        result = subprocess.run(
            [
                "robocopy",
                str(empty_dir),
                str(path),
                "/MIR",
                "/NFL",
                "/NDL",
                "/NJH",
                "/NJS",
                "/nc",
                "/ns",
                "/np",
            ],
            capture_output=True,
            text=True,
        )

        # robocopy exit codes: 0-7 are success (0=no files, 1=files copied, etc.)
        if result.returncode <= 7:
            print(f"✅ Mirrored empty directory (exit code: {result.returncode})")

            # Now remove the empty directory
            try:
                path.rmdir()
                print(f"✅ Removed directory: {path}")
                return True
            except OSError as e:
                print(f"⚠️  Directory removal failed (but should be empty): {e}")
                return False
        else:
            print(f"❌ robocopy failed (exit code: {result.returncode})")
            print(f"stderr: {result.stderr}")
            return False

    finally:
        # Clean up temp directory
        try:
            empty_dir.rmdir()
        except OSError:
            pass


def main() -> int:
    """Main entry point."""
    # Find .fbuild directory
    project_dir = Path(__file__).parent
    fbuild_dir = project_dir / ".fbuild"

    print(f"Project directory: {project_dir}")
    print(f"Cleaning: {fbuild_dir}")

    if not fbuild_dir.exists():
        print("✅ No .fbuild directory found - already clean")
        return 0

    success = remove_with_robocopy(fbuild_dir)

    if success:
        print("\n✅ Build state cleaned successfully")
        return 0
    else:
        print("\n❌ Failed to clean build state - manual intervention may be required")
        print("\nAlternative: Use Windows Explorer to rename .fbuild to a shorter name,")
        print("then delete the renamed directory.")
        return 1


if __name__ == "__main__":
    sys.exit(main())
