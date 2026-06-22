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

Locating the built binary
-------------------------

Cargo writes the `fbuild` executable to either

  <target>/release/fbuild[.exe]

(when no host triple is configured) or

  <target>/<host-triple>/release/fbuild[.exe]

(when soldr / zccache sets `CARGO_BUILD_TARGET=<host-triple>` to isolate
its caches by host — which is what happens in this repo by default). The
previous version of this file only checked the first path; on Windows
where soldr was configured for a host triple, that path was empty even
on a green build, so every `pip install .` failed with

  ERROR: cargo build succeeded but binary not at target\release\fbuild.exe.

To handle both layouts (and any future per-feature/per-profile target
directory) we drive cargo with `--message-format=json-render-diagnostics`
and pull the real artifact path out of cargo's structured output. That's
how `cargo install` and most Rust packaging tools find their binaries.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Optional

from setuptools import setup
from setuptools.command.build_py import build_py
from setuptools.dist import Distribution


REPO_ROOT = Path(__file__).resolve().parent
TARGET_BINARY_NAME = "fbuild.exe" if sys.platform == "win32" else "fbuild"
STAGED_BIN_DIR = REPO_ROOT / "ci" / "bin"
STAGED_BINARY_PATH = STAGED_BIN_DIR / TARGET_BINARY_NAME

# Pin cargo's target directory to a stable absolute path so PEP 517
# isolated builds (pip copies the source tree to a temp dir, so
# `<cwd>/target/` lives in that temp dir and is discarded after the
# build) reuse cargo's incremental fingerprint cache across invocations.
# Without this, every isolated `pip install .` runs cargo cold — 25-30s
# wall-clock per invocation.
#
# We deliberately do NOT share `<repo>/target/` with the dev CLI: the
# dev CLI often runs `cargo check` or different `--features`/`--profile`
# combos, and sharing the target dir means each `pip install` invalidates
# whatever the dev CLI just compiled (and vice versa). A separate
# wheel-build target dir gives both paths a stable, hot cache.
WHEEL_BUILD_TARGET_DIR = Path.home() / ".fbuild" / "cargo-target" / "wheel-build"
os.environ.setdefault("CARGO_TARGET_DIR", str(WHEEL_BUILD_TARGET_DIR))


def _iter_cargo_inputs() -> "list[Path]":
    """Files that, if newer than the staged binary, invalidate the cached build."""
    patterns = (
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        "crates/**/Cargo.toml",
        "crates/**/*.rs",
    )
    paths: list[Path] = []
    for pat in patterns:
        paths.extend(REPO_ROOT.glob(pat))
    return paths


def _staged_binary_is_up_to_date() -> bool:
    """True if the staged binary exists and is newer than every cargo input."""
    if not STAGED_BINARY_PATH.is_file():
        return False
    staged_mtime = STAGED_BINARY_PATH.stat().st_mtime
    for path in _iter_cargo_inputs():
        try:
            if path.stat().st_mtime > staged_mtime:
                return False
        except FileNotFoundError:
            # File disappeared between glob and stat — treat as changed.
            return False
    return True


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


def _find_fbuild_executable_from_json(stdout: str) -> Optional[Path]:
    """Walk cargo's structured artifact stream and return the path to the
    `fbuild` binary, or `None` if no compiler-artifact line for it appeared.

    cargo emits one JSON object per line; the artifact we want has
    `reason == "compiler-artifact"`, `target.name == "fbuild"`, and a non-
    null `executable` field. We keep the *last* match because cargo emits
    one artifact per crate target kind and the bin artifact is what we
    want (matches `cargo install`'s selection rule).
    """
    binary_path: Optional[Path] = None
    for line in stdout.splitlines():
        line = line.strip()
        if not line or not line.startswith("{"):
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            # Non-JSON noise (rare; cargo's renderer can inline human-
            # readable progress on stdout when there's no compatible TTY).
            continue
        if msg.get("reason") != "compiler-artifact":
            continue
        target = msg.get("target") or {}
        if target.get("name") != "fbuild":
            continue
        executable = msg.get("executable")
        if executable:
            binary_path = Path(executable)
    return binary_path


def _use_release_profile() -> bool:
    """True when this build should produce a release-optimized binary.

    Default is `False` — pip/uv-driven builds use the dev profile so the
    iteration loop is fast (workspace's third-party deps stay at opt-level
    3 via `[profile.dev.package."*"]`, only our own crates compile
    unoptimized). Set `FBUILD_BUILD_RELEASE=1` to opt into a release
    build when you actually want a fast binary (CI, packaging, perf
    tests).
    """
    return os.environ.get("FBUILD_BUILD_RELEASE", "").lower() in ("1", "true", "yes")


def _profile_subdir() -> str:
    return "release" if _use_release_profile() else "debug"


def _find_fbuild_executable_by_search() -> Optional[Path]:
    """Fallback when cargo didn't emit a usable artifact line (e.g. a fully
    cached build that reports `Fresh` and skips compiler-artifact). Probe
    the canonical `target/<profile>` path and every per-host-triple subdir.
    """
    profile_dir = _profile_subdir()
    target_root = Path(os.environ.get("CARGO_TARGET_DIR", REPO_ROOT / "target"))
    candidates = [target_root / profile_dir / TARGET_BINARY_NAME]
    if target_root.is_dir():
        for child in target_root.iterdir():
            candidate = child / profile_dir / TARGET_BINARY_NAME
            if candidate.is_file():
                candidates.append(candidate)
    for candidate in candidates:
        if candidate.is_file():
            return candidate
    return None


def _build_fbuild_cli() -> Path:
    """Run `soldr cargo build` and return the path to the built executable."""
    cmd = [
        "soldr",
        "cargo",
        "build",
        "-p",
        "fbuild-cli",
        "--message-format=json-render-diagnostics",
    ]
    if _use_release_profile():
        cmd.insert(3, "--release")
    sys.stderr.write(f"  $ {' '.join(cmd)}\n")
    # stderr passes through so soldr's session summary stays visible; stdout
    # is captured because that's where cargo writes its JSON artifact stream.
    proc = subprocess.run(
        cmd,
        cwd=str(REPO_ROOT),
        stdout=subprocess.PIPE,
        stderr=None,
        check=False,
        text=True,
        encoding="utf-8",
    )
    if proc.returncode != 0:
        sys.stderr.write(
            f"ERROR: `soldr cargo build` exited with code {proc.returncode}.\n"
        )
        sys.exit(proc.returncode)

    binary_path = _find_fbuild_executable_from_json(proc.stdout)
    if binary_path is None or not binary_path.is_file():
        binary_path = _find_fbuild_executable_by_search()

    if binary_path is None or not binary_path.is_file():
        sys.stderr.write(
            "ERROR: cargo build succeeded but no `fbuild` binary was found.\n"
            "Searched:\n"
            "  - cargo's structured JSON artifact stream\n"
            "    (--message-format=json-render-diagnostics)\n"
            f"  - {REPO_ROOT / 'target' / 'release' / TARGET_BINARY_NAME}\n"
            f"  - {REPO_ROOT / 'target'}/<host-triple>/release/{TARGET_BINARY_NAME}\n"
            "If you suspect cargo wrote the binary somewhere else, please\n"
            "file an issue at https://github.com/FastLED/fbuild/issues and\n"
            "attach the output of `soldr cargo build --release -p fbuild-cli -v`.\n"
        )
        sys.exit(1)

    return binary_path


class BuildWithCargo(build_py):
    """Run `soldr cargo build --release -p fbuild-cli` before packaging."""

    def run(self) -> None:  # noqa: D401 — setuptools API name
        # Fast path: if the staged binary is newer than every cargo input,
        # skip the cargo invocation entirely. Even cargo's "Fresh" pass walks
        # the workspace and takes wall-clock seconds; this short-circuits it.
        # Triggered when uv/pip reinstall fbuild without any actual source
        # change (e.g. version bump, lockfile churn, --reinstall-package).
        if _staged_binary_is_up_to_date():
            sys.stderr.write(
                f"  staged binary up-to-date ({STAGED_BINARY_PATH}); skipping cargo\n"
            )
            super().run()
            return

        _require_soldr()
        binary_path = _build_fbuild_cli()
        STAGED_BIN_DIR.mkdir(parents=True, exist_ok=True)
        shutil.copy2(binary_path, STAGED_BINARY_PATH)
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
