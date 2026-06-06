"""Local source-install build driver for fbuild.

`pip install ~/dev/fbuild` (or any `pip install .` from the repo root) goes
through this file because `pyproject.toml` declares the setuptools build
backend. The plain backend would ship only `ci/` Python helpers — no working
`fbuild` command — because the actual CLI is a Rust crate (`fbuild-cli`)
that lives in the cargo workspace under `crates/`.

This file wires the install path through `soldr cargo build --release -p
fbuild-cli`, copies the resulting binary to `ci/bin/fbuild[.exe]`, and lets
setuptools pack it into the wheel. The `ci.bin_launcher:main` entry point
(declared in pyproject.toml) execs that binary, so `fbuild` on PATH after
install Just Works.

This is the LOCAL DEV install path. The RELEASE path lives entirely in
the Autonomous Release GitHub Action (`.github/workflows/release-auto.yml`):
the action builds per-platform binaries on its own runners, calls
`ci/publish.py::build_all_wheels` to assemble platform-tagged wheels, and
uploads to PyPI via trusted publishing (OIDC). See `docs/RELEASING.md`.

Why soldr (and not bare cargo)?

- soldr resolves the toolchain via `rustup which`, respecting
  `rust-toolchain.toml` without requiring PATH to be pre-shaped.
- soldr auto-sets `RUSTC_WRAPPER` to zccache, so rebuilds across `pip
  install .` invocations are incremental + dep-cached.

Why not `setuptools-rust` or `maturin`? Both are reasonable but heavier:
they introduce another tool with its own toolchain assumptions, while
soldr is already the canonical build driver across this repo's dev,
trampoline, and CI paths (see `ci/trampoline.py`). Keeping the single
soldr-cargo invocation means there's only one place to look when iteration
is slow.
"""

from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

from setuptools import setup
from setuptools.command.build_py import build_py
from setuptools.dist import Distribution


REPO_ROOT = Path(__file__).resolve().parent
TARGET_BINARY_NAME = "fbuild.exe" if sys.platform == "win32" else "fbuild"
TARGET_BINARY_PATH = REPO_ROOT / "target" / "release" / TARGET_BINARY_NAME
STAGED_BIN_DIR = REPO_ROOT / "ci" / "bin"
STAGED_BINARY_PATH = STAGED_BIN_DIR / TARGET_BINARY_NAME


def _require_soldr() -> None:
    if shutil.which("soldr") is None:
        sys.stderr.write(
            "\n"
            "ERROR: `soldr` is required to build fbuild from source.\n"
            "Install one of:\n"
            "  uv tool install soldr\n"
            "  curl -fsSL https://raw.githubusercontent.com/zackees/soldr/main/install.sh | bash\n"
            "Then re-run `pip install .`.\n"
            "\n"
            "If you only want the Python helpers (no `fbuild` CLI), install\n"
            "the `fbuild-dev-tools` subpackage instead: `uv sync` from this\n"
            "repo root.\n"
            "\n"
        )
        sys.exit(1)


class BuildWithCargo(build_py):
    """Run `soldr cargo build --release -p fbuild-cli` before packaging."""

    def run(self) -> None:  # noqa: D401 — setuptools API name
        _require_soldr()

        cmd = ["soldr", "cargo", "build", "--release", "-p", "fbuild-cli"]
        sys.stderr.write(f"  $ {' '.join(cmd)}\n")
        subprocess.check_call(cmd, cwd=str(REPO_ROOT))

        if not TARGET_BINARY_PATH.exists():
            sys.stderr.write(f"ERROR: cargo build succeeded but binary not at {TARGET_BINARY_PATH}.\n")
            sys.exit(1)

        STAGED_BIN_DIR.mkdir(parents=True, exist_ok=True)
        shutil.copy2(TARGET_BINARY_PATH, STAGED_BINARY_PATH)
        sys.stderr.write(f"  staged binary -> {STAGED_BINARY_PATH}\n")

        super().run()


class BinaryDistribution(Distribution):
    """Force a platform-tagged wheel because we ship a native binary."""

    def has_ext_modules(self) -> bool:  # noqa: D401 — setuptools API name
        return True


setup(
    cmdclass={"build_py": BuildWithCargo},
    distclass=BinaryDistribution,
)
