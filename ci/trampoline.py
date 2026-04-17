"""Rust toolchain trampolines.

Routes cargo/rustc/rustfmt/clippy-driver through soldr so the
rustup-managed toolchain is always used, without per-call PATH
munging. Registered as project scripts in pyproject.toml so they can
be invoked via `uv run cargo ...`, `uv run rustfmt ...`, etc.

Why soldr:
- soldr resolves each tool via `rustup which`, which respects
  `rust-toolchain.toml` the same way the old PATH-based trampolines
  did, but without requiring PATH to be pre-shaped.
- `soldr --no-cache cargo` preserves the prior bare-cargo semantics
  (no RUSTC_WRAPPER, no managed zccache) so this migration is
  behavior-preserving for CI and local dev. Adopting soldr's built-in
  zccache wrapper is a separate, deliberate decision.
"""

import shutil
import subprocess
import sys
from pathlib import Path


def _soldr_prefix(no_cache: bool):
    """Return the argv prefix that runs soldr, with `--no-cache` if asked."""
    if not shutil.which("soldr"):
        print(
            "error: `soldr` not found on PATH. Run ./install (or `uv sync`) "
            "to install fbuild-dev-tools, which pulls soldr in as a dependency.",
            file=sys.stderr,
        )
        sys.exit(1)
    prefix = ["soldr"]
    if no_cache:
        prefix.append("--no-cache")
    return prefix


def _run_via_soldr(subcommand: str, *, no_cache: bool):
    """Exec `soldr [--no-cache] <subcommand> <argv...>`."""
    cmd = _soldr_prefix(no_cache) + [subcommand] + sys.argv[1:]
    result = subprocess.run(cmd)
    sys.exit(result.returncode)


def cargo():
    # --no-cache keeps soldr's RUSTC_WRAPPER / zccache path off, matching
    # the previous bare-cargo behavior of this trampoline.
    _run_via_soldr("cargo", no_cache=True)


def rustc():
    _run_via_soldr("rustc", no_cache=False)


def rustfmt():
    _run_via_soldr("rustfmt", no_cache=False)


def clippy_driver():
    _run_via_soldr("clippy-driver", no_cache=False)


def _run_cargo_bin(package):
    """Run a cargo binary with the correct toolchain via soldr."""
    extra = sys.argv[1:]
    # Strip leading '--' that uv inserts.
    if extra and extra[0] == "--":
        extra = extra[1:]
    cmd = _soldr_prefix(no_cache=True) + ["cargo", "run", "-p", package]
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
