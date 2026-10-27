"""Check the complexity ratchet on every tracked Python file (fbuild#1639)."""

from __future__ import annotations

import argparse
import subprocess
import sys
import tempfile
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--repo", type=Path, default=Path(__file__).resolve().parents[1]
    )
    root = parser.parse_args().repo.resolve()
    with tempfile.TemporaryFile() as output:
        result = subprocess.run(
            ["git", "ls-files", "-z", "--", "*.py"],
            cwd=root,
            stdout=output,
            stderr=subprocess.STDOUT,
            check=False,
        )
        output.seek(0)
        listing = output.read()
    if result.returncode:
        print(listing.decode("utf-8", errors="replace"), file=sys.stderr)
        return result.returncode
    paths = [path.decode("utf-8") for path in listing.split(b"\0") if path]
    if not paths:
        print("No tracked Python inputs found", file=sys.stderr)
        return 1
    return subprocess.run(
        [
            "uv",
            "run",
            "--no-project",
            "--with",
            "ruff==0.12.12",
            "ruff",
            "check",
            *paths,
        ],
        cwd=root,
        check=False,
    ).returncode


if __name__ == "__main__":
    raise SystemExit(main())
