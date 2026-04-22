"""Repo-local development command helpers.

Rust tooling should be invoked with `soldr ...` or `uv run soldr ...`
directly. This module only keeps helper entry points that run fbuild
workspace binaries through soldr-managed Cargo.

Why soldr:
- soldr resolves each tool via `rustup which`, which respects
  `rust-toolchain.toml` without requiring PATH to be pre-shaped.
- The normal Cargo path is `soldr cargo ...`, so local dev and CI get
  soldr's managed zccache path by default without repo-specific
  `RUSTC_WRAPPER` wiring.
"""

import shutil
import subprocess
import sys
from pathlib import Path


def _soldr_prefix():
    """Return the argv prefix that runs soldr."""
    if not shutil.which("soldr"):
        print(
            "error: `soldr` not found on PATH. Run ./install (or `uv sync`) "
            "to install fbuild-dev-tools, which pulls soldr in as a dependency.",
            file=sys.stderr,
        )
        sys.exit(1)
    return ["soldr"]


def _run_workspace_package(package):
    """Run a cargo binary with the correct toolchain via soldr."""
    extra = sys.argv[1:]
    # Strip leading '--' that uv inserts.
    if extra and extra[0] == "--":
        extra = extra[1:]
    cmd = _soldr_prefix() + ["cargo", "run", "-p", package]
    if extra:
        cmd.append("--")
        cmd.extend(extra)
    result = subprocess.run(cmd)
    sys.exit(result.returncode)


def run_fbuild():
    _run_workspace_package("fbuild-cli")


def run_fbuild_daemon():
    _run_workspace_package("fbuild-daemon")


def publish():
    """Run the publish pipeline via the root publish script."""
    script = Path(__file__).resolve().parent.parent / "publish"
    result = subprocess.run([sys.executable, str(script)] + sys.argv[1:])
    sys.exit(result.returncode)
