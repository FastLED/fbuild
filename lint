#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# ///
"""Run workspace linting: rustfmt check + clippy.

Usage:
    ./lint              # full workspace lint
    ./lint --fix        # auto-fix formatting + clippy
    ./lint <file.rs>    # single-file rustfmt + per-crate clippy
"""

import os
import subprocess
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).parent.resolve()


def run_cmd(cmd):
    """Run a command rooted at the project directory."""
    return subprocess.run(
        cmd,
        text=True,
        encoding="utf-8",
        errors="replace",
        cwd=str(SCRIPT_DIR),
    )


def detect_crate(file_path):
    """Extract crate name from a file path under crates/."""
    normalized = file_path.replace("\\", "/")
    if "crates/" in normalized:
        parts = normalized.split("crates/")
        if len(parts) > 1:
            crate_dir = parts[1].split("/")[0]
            if crate_dir:
                return crate_dir
    return None


def lint_single_file(file_path):
    """Lint a single .rs file: rustfmt + per-crate clippy."""
    file_path = os.path.abspath(file_path)

    if not file_path.endswith(".rs"):
        print(f"Skipping non-Rust file: {file_path}", file=sys.stderr)
        return 0

    if not os.path.isfile(file_path):
        print(f"File not found: {file_path}", file=sys.stderr)
        return 1

    # Format single file
    result = run_cmd(["uv", "run", "rustfmt", file_path])
    if result.returncode != 0:
        return result.returncode

    # Clippy on the affected crate (or workspace if unknown)
    crate = detect_crate(file_path)
    cmd = ["uv", "run", "cargo", "clippy"]
    if crate:
        cmd += ["-p", crate]
    else:
        cmd += ["--workspace"]
    cmd += ["--all-targets", "--", "-D", "warnings"]

    result = run_cmd(cmd)
    return result.returncode


def lint_workspace():
    """Full workspace lint: fmt check + clippy."""
    # Format check
    result = run_cmd(["uv", "run", "cargo", "fmt", "--all", "--check"])
    if result.returncode != 0:
        print("Formatting issues found. Run './lint --fix' to auto-fix.", file=sys.stderr)
        return result.returncode

    # Clippy
    result = run_cmd([
        "uv", "run", "cargo", "clippy", "--workspace", "--all-targets",
        "--", "-D", "warnings",
    ])
    return result.returncode


def main():
    args = sys.argv[1:]

    # Handle --fix flag
    if "--fix" in args:
        args.remove("--fix")
        result = run_cmd(["uv", "run", "cargo", "fmt", "--all"])
        if result.returncode != 0:
            return result.returncode
        if not args:
            # After fixing fmt, run clippy
            result = run_cmd([
                "uv", "run", "cargo", "clippy", "--workspace", "--all-targets",
                "--", "-D", "warnings",
            ])
            return result.returncode

    # Single file mode
    if args and args[0].endswith(".rs"):
        return lint_single_file(args[0])

    # Workspace mode
    return lint_workspace()


if __name__ == "__main__":
    sys.exit(main())
