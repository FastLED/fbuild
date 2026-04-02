#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# ///
"""Run workspace tests.

Usage:
    uv run test                          # unit tests only
    uv run test --full                   # unit + stress + integration tests
    uv run test -p fbuild-core            # single crate
    uv run test -p fbuild-core -- name   # single test by name
"""

import subprocess
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).parent.resolve()


def main():
    cmd = ["uv", "run", "cargo", "test"]

    args = sys.argv[1:]
    full = "--full" in args
    if full:
        args.remove("--full")

    # Split on "--" to separate cargo args from test-binary args.
    if "--" in args:
        sep = args.index("--")
        cargo_args = args[:sep]
        test_args = args[sep + 1:]
    else:
        cargo_args = args
        test_args = []

    if not cargo_args:
        cmd += ["--workspace"]

    cmd += cargo_args
    cmd += ["--"]
    if full:
        cmd += ["--include-ignored"]
    cmd += test_args

    result = subprocess.run(
        cmd,
        text=True,
        encoding="utf-8",
        errors="replace",
        cwd=str(SCRIPT_DIR),
    )
    return result.returncode


if __name__ == "__main__":
    sys.exit(main())
