"""Rust toolchain trampolines.

Ensures the rustup-managed toolchain is on PATH before executing
the real Rust tool. Registered as project scripts in pyproject.toml
so they can be invoked via `uv run cargo ...`, `uv run rustfmt ...`, etc.
"""

import os
import shutil
import subprocess
import sys


def _find_cargo_bin():
    """Find the rustup .cargo/bin directory."""
    for candidate in [
        os.environ.get("CARGO_HOME", ""),
        os.path.join(os.path.expanduser("~"), ".cargo"),
        os.path.join(os.environ.get("USERPROFILE", ""), ".cargo"),
    ]:
        if candidate:
            bin_dir = os.path.join(candidate, "bin")
            if os.path.isdir(bin_dir):
                return bin_dir
    return None


def _run_tool(tool_name):
    """Prepend .cargo/bin to PATH and exec the given tool."""
    cargo_bin = _find_cargo_bin()
    if not cargo_bin:
        print("error: Cannot find .cargo/bin. Run ./install first.", file=sys.stderr)
        sys.exit(1)

    os.environ["PATH"] = cargo_bin + os.pathsep + os.environ.get("PATH", "")

    if not shutil.which(tool_name):
        print(f"error: {tool_name} not found in {cargo_bin}.", file=sys.stderr)
        sys.exit(1)

    result = subprocess.run([tool_name] + sys.argv[1:])
    sys.exit(result.returncode)


def cargo():
    _run_tool("cargo")


def rustc():
    _run_tool("rustc")


def rustfmt():
    _run_tool("rustfmt")


def clippy_driver():
    _run_tool("clippy-driver")


def _run_cargo_bin(package):
    """Run a cargo binary with the correct toolchain on PATH."""
    cargo_bin = _find_cargo_bin()
    if not cargo_bin:
        print("error: Cannot find .cargo/bin. Run ./install first.", file=sys.stderr)
        sys.exit(1)

    os.environ["PATH"] = cargo_bin + os.pathsep + os.environ.get("PATH", "")

    extra = sys.argv[1:]
    # Strip leading '--' that uv inserts
    if extra and extra[0] == "--":
        extra = extra[1:]
    cmd = ["cargo", "run", "-p", package]
    if extra:
        cmd.append("--")
        cmd.extend(extra)
    result = subprocess.run(cmd)
    sys.exit(result.returncode)


def run_fbuild():
    _run_cargo_bin("fbuild-cli")


def run_fbuild_daemon():
    _run_cargo_bin("fbuild-daemon")
