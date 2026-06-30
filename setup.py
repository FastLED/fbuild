"""Local source-install build driver for fbuild.

`pip install ~/dev/fbuild` (or any `pip install .` from the repo root) goes
through this file because `pyproject.toml` declares the setuptools build
backend. The plain backend would ship only the `python/fbuild` Python
package — no working `fbuild` command — because the actual CLI is a Rust
crate (`fbuild-cli`) that lives in the cargo workspace under `crates/`.

This file wires the install path through `soldr cargo build --release -p
fbuild-cli`, copies the resulting binary to `ci/bin/fbuild[.exe]`, and
hands that path to setuptools as a raw wheel script (the `scripts=`
argument to `setup()` below). Pip drops raw scripts straight into the
venv's `Scripts/` (Windows) or `bin/` (POSIX) directory as-is — `.exe`
files are NOT wrapped, so `fbuild` on PATH is the literal cargo-built
binary with no Python shim in front of it (see #746 for why the previous
`[project.scripts] fbuild = "ci.bin_launcher:main"` approach broke stdout
ordering on Windows).

This is the LOCAL DEV install path. The RELEASE path lives entirely in
the Autonomous Release GitHub Action (`.github/workflows/release-auto.yml`):
the action builds per-platform binaries on its own runners, calls
`ci/publish.py::build_all_wheels` to assemble platform-tagged wheels, and
uploads to PyPI via trusted publishing (OIDC). See `docs/RELEASING.md`.

Why soldr (and not bare cargo)?

- soldr resolves the toolchain via `rustup which`, respecting
  `rust-toolchain.toml` without requiring PATH to be pre-shaped.
- soldr auto-sets `RUSTC_WRAPPER` to itself (or the dedicated
  `zccache-soldr` shim per soldr#1081), which talks to soldr-daemon's
  embedded zccache (soldr#977 / #980 L1) — rebuilds across `pip install .`
  invocations are incremental + dep-cached. There is no standalone
  `zccache.exe` involved; the wrapper chain is entirely in-soldr.

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

# Import setuptools FIRST so its distutils shim is installed before we
# pull `distutils.command.build_scripts` off the shim. Importing the
# distutils module without setuptools loaded first either misses the
# shim or (depending on Python/setuptools version) yields the stdlib
# distutils, which is deprecated and gone in Python 3.12+.
from setuptools import setup
from setuptools.command.build_py import build_py
from setuptools.dist import Distribution
from distutils.command.build_scripts import (  # type: ignore[import-untyped]
    build_scripts,
)


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


class BuildBinaryScripts(build_scripts):
    """Byte-copy variant of `build_scripts` for raw native binaries.

    Stock `build_scripts.copy_scripts` calls `tokenize.open(script)` on
    each entry to detect a coding cookie and patch a shebang for source
    scripts. That's right for `.py` files but wrong for a Rust-built
    `.exe` / ELF binary — `tokenize.open` raises `SyntaxError: invalid or
    missing encoding declaration` on the very first read. We override
    `copy_scripts` to do a plain byte-level `shutil.copy2`, preserving
    the executable bit on POSIX (cargo already sets it). The file lands
    in `<name>-<version>.data/scripts/` in the wheel; pip then copies it
    straight into the install's `Scripts/` (Windows) or `bin/` (POSIX)
    directory verbatim — no shebang, no Python wrapper. See #746.
    """

    def copy_scripts(self):  # noqa: D401 — distutils API name
        self.mkpath(self.build_dir)
        outfiles: list[str] = []
        updated_files: list[str] = []
        for script in self.scripts:
            outfile = os.path.join(self.build_dir, os.path.basename(script))
            # `dep_util.newer` returns True if `script` is newer than
            # `outfile`, mirroring stock build_scripts' "update or skip"
            # behavior — avoids spurious rebuilds breaking caching.
            try:
                from distutils import dep_util  # type: ignore[import-untyped]

                up_to_date = (
                    os.path.exists(outfile) and not dep_util.newer(script, outfile)
                )
            except ImportError:
                # Python 3.12+ removed distutils.dep_util; fall back to
                # an mtime compare.
                up_to_date = os.path.exists(outfile) and (
                    os.path.getmtime(script) <= os.path.getmtime(outfile)
                )
            if up_to_date and not self.force:
                outfiles.append(outfile)
                continue
            shutil.copy2(script, outfile)
            outfiles.append(outfile)
            updated_files.append(outfile)
        return outfiles, updated_files


# `scripts=` is the legacy setuptools mechanism for shipping raw files
# (no shebang/no entry-point wrapping) into the install's Scripts/bin
# directory. Files land in `<name>-<version>.data/scripts/` inside the
# wheel; `pip install` then copies them straight into the venv as-is —
# on Windows pip does NOT generate a Python wrapper for `.exe` files
# (only for shebang-style script text). This is the same mechanism
# maturin's "bin" mode and cargo-dist use to ship native binaries via
# PyPI without a Python shim. See #746.
#
# Stock `build_scripts` parses each script as Python source to find a
# coding cookie / shebang — we override with `BuildBinaryScripts` to do
# a plain byte-copy instead. `STAGED_BINARY_PATH` doesn't exist until
# `BuildWithCargo` runs, which happens during the `build_py` phase.
# Setuptools' build pipeline runs `build_py` before `build_scripts`, so
# by the time `build_scripts` reads this list, the file is on disk.
setup(
    cmdclass={
        "build_py": BuildWithCargo,
        "build_scripts": BuildBinaryScripts,
    },
    distclass=BinaryDistribution,
    scripts=[str(STAGED_BINARY_PATH)],
)
