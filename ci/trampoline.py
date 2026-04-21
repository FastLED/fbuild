"""Rust toolchain trampolines.

Routes cargo/rustc/rustfmt/clippy-driver through soldr so the
rustup-managed toolchain is always used, without per-call PATH
munging. Registered as project scripts in pyproject.toml so they can
be invoked via `uv run cargo ...`, `uv run rustfmt ...`, etc.

Why soldr:
- soldr resolves each tool via `rustup which`, which respects
  `rust-toolchain.toml` the same way the old PATH-based trampolines
  did, but without requiring PATH to be pre-shaped.
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


def _run_via_soldr(subcommand: str):
    """Exec `soldr <subcommand> <argv...>`."""
    cmd = _soldr_prefix() + [subcommand] + sys.argv[1:]
    result = subprocess.run(cmd)
    sys.exit(result.returncode)


def cargo():
    _run_via_soldr("cargo")


def rustc():
    _run_via_soldr("rustc")


def rustfmt():
    _run_via_soldr("rustfmt")


def clippy_driver():
    _run_via_soldr("clippy-driver")


def _run_cargo_bin(package):
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
    _run_cargo_bin("fbuild-cli")


def run_fbuild_daemon():
    _run_cargo_bin("fbuild-daemon")


def publish():
    """Run the publish pipeline via the root publish script."""
    script = Path(__file__).resolve().parent.parent / "publish"
    result = subprocess.run([sys.executable, str(script)] + sys.argv[1:])
    sys.exit(result.returncode)
