"""Check the complexity ratchet on every tracked Python file (fbuild#1639).

The ratchet is configured in ``pyproject.toml`` (``[tool.ruff.lint]`` selects
exactly ``C901``/``RUF100`` and ``[tool.ruff.lint.mccabe]`` pins
``max-complexity = 10``).  This module only decides *which files* are checked:
every ``git ls-files`` Python path, which includes the scripts normal discovery
skips (``ci/build_backend.py`` and friends are not ``test_*.py``).

``ci/test_check_python_complexity.py`` drives the same seams end to end, so a
loosened threshold or a dropped rule is a test failure instead of a silent
policy drift.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
import tempfile
from collections.abc import Sequence
from pathlib import Path

RUFF_VERSION = "ruff==0.12.12"


class RatchetError(RuntimeError):
    """The file list itself could not be established."""


def tracked_python_files(root: Path) -> list[str]:
    """Every Python path tracked by git under ``root``, in git's order."""

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
        raise RatchetError(listing.decode("utf-8", errors="replace"))
    paths = [path.decode("utf-8") for path in listing.split(b"\0") if path]
    if not paths:
        raise RatchetError("No tracked Python inputs found")
    return paths


def ruff_command(paths: Sequence[str]) -> list[str]:
    """The pinned ruff invocation; ``paths`` are relative to the check root."""

    return [
        "uv",
        "run",
        "--no-project",
        "--with",
        RUFF_VERSION,
        "ruff",
        "check",
        *paths,
    ]


def check(root: Path) -> int:
    """Run the ratchet over ``root`` and return the ruff exit code."""

    try:
        paths = tracked_python_files(root)
    except RatchetError as error:
        print(error, file=sys.stderr)
        return 1
    return subprocess.run(ruff_command(paths), cwd=root, check=False).returncode


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--repo", type=Path, default=Path(__file__).resolve().parents[1]
    )
    return check(parser.parse_args().repo.resolve())


if __name__ == "__main__":
    raise SystemExit(main())
