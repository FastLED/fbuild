"""Local source-install build driver for fbuild.

`pip install ~/dev/fbuild` (or any `pip install .` / `uv sync` from the repo
root) goes through this in-tree PEP 517 / PEP 660 backend (`[build-system]` in
pyproject.toml, `backend-path = ["ci"]`). Plain Python package discovery would
ship no working `fbuild` command because the actual CLI is a Rust crate
(`fbuild-cli`) that lives in the cargo workspace under `crates/`.

This file wires the install path through `soldr cargo build -p fbuild-cli`
(or `--release` when requested), stages the resulting binaries in
`ci/bin/`, and writes them into the wheel's `.data/scripts/`. Pip drops raw
scripts straight into the venv's `Scripts/` (Windows) or `bin/` (POSIX)
directory as-is, so `fbuild` on PATH is the literal cargo-built binary with
no Python shim in front of it (see #746).

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

Why not setuptools, `setuptools-rust` or `maturin`? Both are reasonable but heavier:
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

When the staged artifacts may be reused
---------------------------------------

Staging lives in `ci/bin/` and a warm tree skips cargo entirely, so the cache
key has to carry everything that changes the *bytes*:

  * the mtime of every cargo input (`.rs`, `Cargo.toml`, `Cargo.lock`, the
    toolchain pin) -- a staged artifact older than any input is rebuilt;
  * the cargo **profile**, recorded in `ci/bin/.build-profile`. The staging
    paths are profile-agnostic, so without that stamp `FBUILD_BUILD_RELEASE=1`
    after a plain `pip install .` (or the reverse) reused the other profile;
  * non-emptiness. A zero-byte artifact is never "current": copies are staged
    through a temp file + `os.replace`, so an interrupted install can only
    leave a temp behind, never a truncated `ci/bin/fbuild` that later runs
    would adopt as fresh and package into every wheel.
"""

from __future__ import annotations

import argparse
import base64
import csv
import hashlib
import io
import json
import re
import stat
import sysconfig
import zipfile
import os
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

REPO_ROOT = Path(__file__).resolve().parent.parent
TARGET_BINARY_NAME = "fbuild.exe" if sys.platform == "win32" else "fbuild"
TARGET_DAEMON_NAME = "fbuild-daemon.exe" if sys.platform == "win32" else "fbuild-daemon"
STAGED_BIN_DIR = REPO_ROOT / "ci" / "bin"
STAGED_BINARY_PATH = STAGED_BIN_DIR / TARGET_BINARY_NAME
STAGED_DAEMON_PATH = STAGED_BIN_DIR / TARGET_DAEMON_NAME
# Records which cargo profile (debug/release) produced the staged files;
# the staging paths themselves are profile-agnostic.
STAGED_PROFILE_PATH = STAGED_BIN_DIR / ".build-profile"

# FastLED/fbuild#829: PyO3 extension produced by `fbuild-python`. Python
# extension module conventions: `.pyd` on Windows, `.so` on Linux/macOS.
# Cargo emits a cdylib named `lib_native.{so,dylib}` on Unix or
# `_native.dll` on Windows; we rename / strip-prefix during the copy.
PYTHON_EXT_DIR = REPO_ROOT / "python" / "fbuild"
if sys.platform == "win32":
    STAGED_NATIVE_EXT_NAME = "_native.pyd"
else:
    # Same `.so` suffix on macOS and Linux — what CPython searches for.
    STAGED_NATIVE_EXT_NAME = "_native.so"
STAGED_NATIVE_EXT_PATH = PYTHON_EXT_DIR / STAGED_NATIVE_EXT_NAME


# Pin cargo's target directory to a persistent path keyed by this repo's
# root. The build never runs cargo in a temp copy of the tree (which would
# start cold every time); instead every checkout gets its own stable,
# warm target dir under ~/.fbuild, and two checkouts never thrash each
# other's fingerprints.
#
# We deliberately do NOT share `<repo>/target/` with the dev CLI: the
# dev CLI often runs `cargo check` or different `--features`/`--profile`
# combos, and sharing the target dir means each `pip install` invalidates
# whatever the dev CLI just compiled (and vice versa).
def _wheel_build_target_dir(repo_root: Path) -> Path:
    digest = hashlib.sha256(str(repo_root).encode("utf-8")).hexdigest()[:12]
    return (
        Path.home()
        / ".fbuild"
        / "cargo-target"
        / "wheel-build"
        / f"{repo_root.name}-{digest}"
    )


WHEEL_BUILD_TARGET_DIR = _wheel_build_target_dir(REPO_ROOT)
os.environ.setdefault("CARGO_TARGET_DIR", str(WHEEL_BUILD_TARGET_DIR))

# FastLED/fbuild#2062: the store above is immortal. `_wheel_build_target_dir`
# keys the directory on `sha256(str(repo_root))`, which is one-way, so once a
# checkout or agent worktree is deleted nothing left in the store can say which
# path a target belonged to -- let alone whether that path still exists. The
# directory is never reclaimed either, so the store grows one full release
# target (0.06-4 GB) per worktree ever created: 38 entries / 52 GB, of which
# 17 / 31.8 GB were orphans, on the box this was filed from.
#
# Two halves close it:
#
#   * every target records its owning checkout in `.fbuild-wheel-source`, so
#     liveness is one `Path.exists()` instead of a guess, and a diagnostic can
#     name the checkout a target belongs to;
#   * a build sweeps sibling targets whose recorded owner is GONE (exact
#     evidence, no age needed) or, for targets written before the stamp
#     existed, whose newest file is older than the idle gate.
#
# Target dirs are caches, so deleting a live one only costs a rebuild -- but
# "cannot tell" still means "keep": an unreadable entry, a scan over budget and
# a symlinked entry all survive untouched.
WHEEL_BUILD_SOURCE_STAMP = ".fbuild-wheel-source"
DEFAULT_WHEEL_BUILD_IDLE_DAYS = 14.0
WHEEL_BUILD_SWEEP_ENV = "FBUILD_WHEEL_BUILD_SWEEP"
WHEEL_BUILD_IDLE_DAYS_ENV = "FBUILD_WHEEL_BUILD_IDLE_DAYS"
# Files scanned inside ONE legacy (unstamped) target before the sweep gives up
# and keeps it. A cargo target dir holds ~10k files; the cap keeps `pip install`
# from spending seconds on a directory whose age it cannot establish cheaply.
WHEEL_BUILD_SCAN_BUDGET = 20_000


@dataclass(frozen=True)
class ReclaimedTarget:
    """One sibling target the sweep dropped (or would drop, under --dry-run)."""

    path: Path
    reason: str
    bytes_freed: int


def _wheel_build_store() -> Path:
    """The `~/.fbuild/cargo-target/wheel-build` store holding every target."""
    return WHEEL_BUILD_TARGET_DIR.parent


def _dir_key(path: Path) -> str:
    """Comparison key for a directory path (case-insensitive on Windows)."""
    return os.path.normcase(os.path.abspath(str(path)))


def record_wheel_build_source(target: Path, root: Path) -> None:
    """Record `root` as the checkout owning `target` (idempotent, best-effort).

    Written into the target dir itself so the store stays self-describing. A
    failure here is a lost diagnostic, never a failed install.
    """
    stamp = target / WHEEL_BUILD_SOURCE_STAMP
    text = f"{_dir_key(root)}\n"
    try:
        if stamp.read_text(encoding="utf-8") == text:
            return
    except OSError:
        pass
    try:
        target.mkdir(parents=True, exist_ok=True)
        stamp.write_text(text, encoding="utf-8")
    except OSError:
        pass


def _recorded_source(target: Path) -> Optional[str]:
    """The owner path recorded inside `target`, or None when unstamped."""
    try:
        recorded = (
            (target / WHEEL_BUILD_SOURCE_STAMP).read_text(encoding="utf-8").strip()
        )
    except OSError:
        return None
    return recorded or None


def _scan_target(
    target: Path, *, budget: int = WHEEL_BUILD_SCAN_BUDGET
) -> Optional[tuple[float, int]]:
    """(newest mtime, total bytes) under `target`; None when over `budget`.

    One metadata walk, no file reads. `os.walk` does not follow symlinks, so a
    linked tree is never counted as part of this target.
    """
    newest = 0.0
    total = 0
    scanned = 0
    for dirpath, _dirnames, filenames in os.walk(target, onerror=lambda _e: None):
        for name in filenames:
            scanned += 1
            if scanned > budget:
                return None
            try:
                info = os.lstat(os.path.join(dirpath, name))
            except OSError:
                continue
            total += info.st_size
            if info.st_mtime > newest:
                newest = info.st_mtime
    return newest, total


def _format_bytes(count: int) -> str:
    """Human-readable size, MB/GB only -- these are build target dirs."""
    value = float(count)
    for unit in ("B", "KB", "MB"):
        if value < 1024.0:
            return f"{value:.0f} {unit}" if unit == "B" else f"{value:.1f} {unit}"
        value /= 1024.0
    return f"{value:.1f} GB"


def _wheel_build_idle_days() -> float:
    """Idle gate for legacy (unstamped) targets; bad values fall back to 14."""
    raw = os.environ.get(WHEEL_BUILD_IDLE_DAYS_ENV, "").strip()
    if not raw:
        return DEFAULT_WHEEL_BUILD_IDLE_DAYS
    try:
        days = float(raw)
    except ValueError:
        return DEFAULT_WHEEL_BUILD_IDLE_DAYS
    return days if days >= 0 else DEFAULT_WHEEL_BUILD_IDLE_DAYS


def _sibling_target_dirs(store: Path, current_key: str) -> "list[Path]":
    """Plain directory children of `store`, minus the caller's own target.

    Symlinks are skipped: `os.scandir(...).is_dir(follow_symlinks=False)` says
    no, so a linked entry can never be walked or deleted through this path.
    """
    siblings: list[Path] = []
    try:
        entries = list(os.scandir(store))
    except OSError:
        return siblings
    for entry in entries:
        try:
            if not entry.is_dir(follow_symlinks=False):
                continue
        except OSError:
            continue
        path = Path(entry.path)
        if _dir_key(path) == current_key:
            continue
        siblings.append(path)
    return siblings


def _reclaim_verdict(
    path: Path, current_key: str, cutoff: float, idle_days: float
) -> "Optional[ReclaimedTarget]":
    """Why `path` is dead, or None when it must be kept."""
    recorded = _recorded_source(path)
    if recorded is not None:
        owner = Path(recorded)
        if _dir_key(owner) == current_key or owner.exists():
            return None
        scan = _scan_target(path)
        return ReclaimedTarget(
            path=path,
            reason=f"owner gone: {recorded}",
            bytes_freed=scan[1] if scan else 0,
        )
    scan = _scan_target(path)
    if scan is None:  # too big to date cheaply: keep it rather than guess
        return None
    newest, total = scan
    if newest >= cutoff:
        return None
    return ReclaimedTarget(
        path=path,
        reason=f"idle > {idle_days:g}d, no owner stamp",
        bytes_freed=total,
    )


def plan_wheel_build_reclaim(
    store: Path,
    current: Path,
    *,
    idle_days: float = DEFAULT_WHEEL_BUILD_IDLE_DAYS,
    now: Optional[float] = None,
) -> "list[ReclaimedTarget]":
    """Decide which sibling targets in `store` are dead. Deletes nothing.

    `current` is the target the caller is about to build into: it is never a
    candidate, whatever its stamp says. A target is doomed when its recorded
    owner no longer exists (or is `current`), or -- for a legacy target with no
    stamp -- when every file in it is older than `idle_days`.
    """
    if idle_days < 0:
        raise ValueError(f"idle_days must be >= 0, got {idle_days}")
    reference = time.time() if now is None else now
    cutoff = reference - idle_days * 86400.0
    current_key = _dir_key(current)
    doomed: list[ReclaimedTarget] = []
    for path in _sibling_target_dirs(store, current_key):
        verdict = _reclaim_verdict(path, current_key, cutoff, idle_days)
        if verdict is not None:
            doomed.append(verdict)
    return doomed


def reclaim_stale_wheel_build_targets(
    store: Optional[Path] = None,
    current: Optional[Path] = None,
    *,
    idle_days: Optional[float] = None,
    dry_run: bool = False,
    now: Optional[float] = None,
) -> "list[ReclaimedTarget]":
    """Delete dead sibling targets; returns the ones actually reclaimed.

    A target is only ever removed when it is a direct child of `store`, so a
    malformed entry can never turn into a delete somewhere else on disk.
    """
    store = _wheel_build_store() if store is None else store
    current = WHEEL_BUILD_TARGET_DIR if current is None else current
    if idle_days is None:
        idle_days = _wheel_build_idle_days()
    doomed = plan_wheel_build_reclaim(store, current, idle_days=idle_days, now=now)
    if dry_run:
        return doomed
    reclaimed: list[ReclaimedTarget] = []
    for victim in doomed:
        if victim.path.parent != store:
            continue
        try:
            shutil.rmtree(victim.path)
        except OSError:
            continue
        reclaimed.append(victim)
    return reclaimed


def _sweep_wheel_build_targets() -> None:
    """Reclaim dead sibling targets before building into this one.

    Env: `FBUILD_WHEEL_BUILD_SWEEP=0` disables it, `FBUILD_WHEEL_BUILD_IDLE_DAYS`
    moves the legacy idle gate (default 14). Never raises: a sweep failure must
    not fail an install.
    """
    raw = os.environ.get(WHEEL_BUILD_SWEEP_ENV, "1").strip().lower()
    if raw in ("0", "false", "no", "off"):
        return
    try:
        reclaimed = reclaim_stale_wheel_build_targets()
    except (OSError, ValueError):
        return
    if not reclaimed:
        return
    freed = sum(target.bytes_freed for target in reclaimed)
    sys.stderr.write(
        f"  reclaimed {_format_bytes(freed)} from {len(reclaimed)} dead "
        f"wheel-build target(s) in {_wheel_build_store()}\n"
    )
    for target in reclaimed:
        sys.stderr.write(f"    - {target.path.name} ({target.reason})\n")


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


def _staged_artifact_is_current(path: Path) -> bool:
    """True if `path` exists, is non-empty, and is newer than every cargo input.

    A zero-byte artifact is never current: `shutil.copy2` writes in place, so an
    install killed mid-copy leaves a truncated file whose mtime is the NEWEST in
    the tree, and the old check then reused it forever while reporting success.
    """
    try:
        staged = path.stat()
    except FileNotFoundError:
        return False
    if staged.st_size == 0:
        return False
    for cargo_input in _iter_cargo_inputs():
        try:
            if cargo_input.stat().st_mtime > staged.st_mtime:
                return False
        except FileNotFoundError:
            # File disappeared between glob and stat — treat as changed.
            return False
    return True


def _current_profile_stamp() -> str:
    """Stamp identifying which cargo profile produced the staged artifacts."""
    return f"v1 {_profile_subdir()}"


def _staged_profile_is_current() -> bool:
    """True when the staged artifacts were built for the profile we now want.

    The staged paths are shared between debug and release, so without this
    stamp `FBUILD_BUILD_RELEASE=1` after a plain `pip install .` (or the
    reverse) silently shipped the other profile's binary -- the wheel's
    staleness check only looked at mtimes.
    """
    try:
        return STAGED_PROFILE_PATH.read_text(encoding="utf-8").strip() == (
            _current_profile_stamp()
        )
    except (FileNotFoundError, OSError):
        return False


def _staged_artifacts_are_current() -> bool:
    """True only when all three staged artifacts match this build request."""
    if not _staged_profile_is_current():
        return False
    return all(
        _staged_artifact_is_current(path)
        for path in (
            STAGED_BINARY_PATH,
            STAGED_DAEMON_PATH,
            STAGED_NATIVE_EXT_PATH,
        )
    )


def _atomic_copy(src: Path, dst: Path) -> None:
    """Copy `src` to `dst` through a sibling temp file + `os.replace`.

    Staging in place means an interrupted install leaves a partial artifact at
    the final path; the next run then sees a fresh mtime and packages it.
    """
    dst.parent.mkdir(parents=True, exist_ok=True)
    tmp = dst.with_name(f"{dst.name}.tmp")
    shutil.copy2(src, tmp)
    os.replace(tmp, dst)


def _write_profile_stamp() -> None:
    """Record which profile the staged artifacts now hold."""
    STAGED_BIN_DIR.mkdir(parents=True, exist_ok=True)
    tmp = STAGED_PROFILE_PATH.with_name(f"{STAGED_PROFILE_PATH.name}.tmp")
    tmp.write_text(f"{_current_profile_stamp()}\n", encoding="utf-8")
    os.replace(tmp, STAGED_PROFILE_PATH)


def _assert_staged_payload() -> None:
    """Refuse to wrap a missing or empty artifact in a wheel."""
    for path in (STAGED_BINARY_PATH, STAGED_DAEMON_PATH, STAGED_NATIVE_EXT_PATH):
        try:
            size = path.stat().st_size
        except FileNotFoundError:
            size = 0
        if size == 0:
            sys.stderr.write(
                f"ERROR: staged artifact {path} is missing or empty; refusing to "
                "build a wheel that would install a broken `fbuild`. Delete "
                f"`{STAGED_BIN_DIR}` and re-run the install.\n"
            )
            sys.exit(1)


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
            "the `ci/dev-tools` subpackage directly instead.\n"
            "\n"
        )
        sys.exit(1)


def _cargo_artifacts(stdout: str):
    """Yield every `compiler-artifact` message in cargo's JSON stream.

    cargo writes one JSON object per line, but the stream also carries
    human-readable progress noise interleaved with it. Every line is offered to
    `json.loads` and skipped unless it decodes to an object: a prefilter like
    `line.startswith("{")` would leave the `isinstance` check below unreachable
    (and therefore untestable) while still being needed for a stream that
    contains `[1, 2]` or a bare string.
    """
    for line in stdout.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(msg, dict):
            continue
        if msg.get("reason") != "compiler-artifact":
            continue
        yield msg


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
    for msg in _cargo_artifacts(stdout):
        target = msg.get("target") or {}
        if target.get("name") != "fbuild":
            continue
        executable = msg.get("executable")
        if executable:
            binary_path = Path(executable)
    return binary_path


def _find_fbuild_daemon_from_json(stdout: str) -> Optional[Path]:
    """Return the `fbuild-daemon` executable from cargo's artifact stream."""
    binary_path: Optional[Path] = None
    for msg in _cargo_artifacts(stdout):
        target = msg.get("target") or {}
        if target.get("name") != "fbuild-daemon":
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
    tests). Source installs can set `FBUILD_BUILD_RELEASE=1` for a release
    build; the setuptools backend itself does not select the Rust profile.
    """
    explicit_release = os.environ.get("FBUILD_BUILD_RELEASE")
    if explicit_release is not None:
        return explicit_release.lower() in ("1", "true", "yes")

    return False


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


def _find_fbuild_daemon_by_search() -> Optional[Path]:
    """Fallback lookup for the companion daemon binary."""
    profile_dir = _profile_subdir()
    target_root = Path(os.environ.get("CARGO_TARGET_DIR", REPO_ROOT / "target"))
    candidates = [target_root / profile_dir / TARGET_DAEMON_NAME]
    if target_root.is_dir():
        for child in target_root.iterdir():
            candidate = child / profile_dir / TARGET_DAEMON_NAME
            if candidate.is_file():
                candidates.append(candidate)
    for candidate in candidates:
        if candidate.is_file():
            return candidate
    return None


def _find_native_cdylib_from_json(stdout: str) -> Optional[Path]:
    """Walk cargo's structured artifact stream and return the path to the
    `_native` cdylib produced by `fbuild-python`.

    FastLED/fbuild#829. Mirrors `_find_fbuild_executable_from_json`'s
    selection rule: keep the *last* `compiler-artifact` for
    `target.name == "_native"` whose `target.kind` includes `cdylib`.
    The `filenames` array carries the cdylib path (cargo doesn't surface
    cdylibs via `executable` since they aren't directly runnable).
    """
    cdylib_path: Optional[Path] = None
    for msg in _cargo_artifacts(stdout):
        target = msg.get("target") or {}
        if target.get("name") != "_native":
            continue
        kinds = target.get("kind") or []
        if "cdylib" not in kinds:
            continue
        for filename in msg.get("filenames") or []:
            path = Path(filename)
            # Pick the cdylib file (suffix varies by host):
            # - Windows: `.dll`
            # - Linux:   `.so`
            # - macOS:   `.dylib`
            if path.suffix in (".dll", ".so", ".dylib"):
                cdylib_path = path
                break
    return cdylib_path


def _find_native_cdylib_by_search() -> Optional[Path]:
    """Fallback when cargo didn't emit a usable artifact line (cached
    `Fresh` builds skip compiler-artifact lines). Probe the canonical
    `target/<profile>` path and every per-host-triple subdir.
    """
    profile_dir = _profile_subdir()
    target_root = Path(os.environ.get("CARGO_TARGET_DIR", REPO_ROOT / "target"))

    if sys.platform == "win32":
        candidate_names = ("_native.dll",)
    elif sys.platform == "darwin":
        candidate_names = ("lib_native.dylib", "lib_native.so")
    else:
        candidate_names = ("lib_native.so",)

    search_dirs = [target_root / profile_dir]
    if target_root.is_dir():
        for child in target_root.iterdir():
            search_dirs.append(child / profile_dir)
    for d in search_dirs:
        for name in candidate_names:
            candidate = d / name
            if candidate.is_file():
                return candidate
    return None


def _build_fbuild_python() -> Path:
    """Run `soldr cargo build -p fbuild-python --features extension-module`
    and return the path to the built cdylib.

    FastLED/fbuild#829: a source/editable install left the Python API
    unusable because `setup.py` only built `fbuild-cli`. The PyO3 extension
    from `crates/fbuild-python` is now built alongside and copied into
    `python/fbuild/_native.{pyd,so}` so `from fbuild import ...` works
    end-to-end after `uv pip install -e .`.
    """
    cmd = [
        "soldr",
        "cargo",
        "build",
        "-p",
        "fbuild-python",
        "--features",
        "extension-module",
        "--message-format=json-render-diagnostics",
    ]
    if _use_release_profile():
        cmd.insert(3, "--release")
    sys.stderr.write(f"  $ {' '.join(cmd)}\n")
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
            f"ERROR: `soldr cargo build -p fbuild-python` exited with code {proc.returncode}.\n"
        )
        sys.exit(proc.returncode)

    cdylib_path = _find_native_cdylib_from_json(proc.stdout)
    if cdylib_path is None or not cdylib_path.is_file():
        cdylib_path = _find_native_cdylib_by_search()

    if cdylib_path is None or not cdylib_path.is_file():
        sys.stderr.write(
            "ERROR: cargo build succeeded but no `_native` cdylib was found.\n"
            "If you suspect cargo wrote the cdylib somewhere else, please\n"
            "file an issue at https://github.com/FastLED/fbuild/issues and\n"
            "attach the output of `soldr cargo build -p fbuild-python "
            "--features extension-module -v`.\n"
        )
        sys.exit(1)

    return cdylib_path


def _build_fbuild_binaries() -> tuple[Path, Path]:
    """Build and return the CLI and its version-matched daemon."""
    cmd = [
        "soldr",
        "cargo",
        "build",
        "-p",
        "fbuild-cli",
        "-p",
        "fbuild-daemon",
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

    daemon_path = _find_fbuild_daemon_from_json(proc.stdout)
    if daemon_path is None or not daemon_path.is_file():
        daemon_path = _find_fbuild_daemon_by_search()
    if daemon_path is None or not daemon_path.is_file():
        sys.stderr.write(
            "ERROR: cargo build succeeded but no `fbuild-daemon` binary was found.\n"
            "A source/editable install must stage CLI and daemon together; refusing\n"
            "to install a combination that can silently run stale deploy code.\n"
        )
        sys.exit(1)

    return binary_path, daemon_path


def _stage_artifacts() -> None:
    """Build (or reuse) the CLI, daemon, and PyO3 extension in their staging paths."""
    # Fast path: when every staged artifact is newer than every cargo input AND
    # was produced for the profile this build asks for, skip cargo entirely.
    # Even cargo's "Fresh" pass walks the workspace and takes wall-clock seconds.
    if _staged_artifacts_are_current():
        sys.stderr.write(
            f"  staged artifacts up-to-date for the {_profile_subdir()} profile "
            f"({STAGED_BINARY_PATH.name}, {STAGED_DAEMON_PATH.name}, "
            f"{STAGED_NATIVE_EXT_PATH.name}); skipping cargo\n"
        )
        _assert_staged_payload()
        return

    _require_soldr()
    # FastLED/fbuild#2062: this directory is a cache keyed on a one-way digest,
    # so it must (a) name its owner and (b) take its dead siblings with it.
    _sweep_wheel_build_targets()
    record_wheel_build_source(WHEEL_BUILD_TARGET_DIR, REPO_ROOT)
    binary_path, daemon_path = _build_fbuild_binaries()
    # Atomic staging: a killed install must not leave a partial artifact that
    # the next run adopts as fresh.
    _atomic_copy(binary_path, STAGED_BINARY_PATH)
    _atomic_copy(daemon_path, STAGED_DAEMON_PATH)
    # FastLED/fbuild#829: the PyO3 extension is staged alongside so
    # `from fbuild import ...` works after a source/editable install.
    cdylib_path = _build_fbuild_python()
    PYTHON_EXT_DIR.mkdir(parents=True, exist_ok=True)
    _atomic_copy(cdylib_path, STAGED_NATIVE_EXT_PATH)
    _write_profile_stamp()
    _assert_staged_payload()
    sys.stderr.write(
        f"  staged {STAGED_BIN_DIR} and {STAGED_NATIVE_EXT_PATH} "
        f"({_profile_subdir()} profile)\n"
    )


# ---------------------------------------------------------------------------
# PEP 517 / PEP 660 backend
#
# A local install used to go through setuptools: pip created an isolated
# build env and installed setuptools into it, ran three setup.py
# subprocesses, copied ~240 MB of binaries into build/ and again into
# build/bdist.*, then zipped them. On a warm tree (cargo skipped) that was
# ~10 s of pure packaging overhead. This backend has no build requirements
# and writes the wheel in one pass, uncompressed: it is a throwaway local
# artifact. Release wheels are assembled by ci/publish.py and stay deflated.
# ---------------------------------------------------------------------------

_NAME = "fbuild"


def _project_field(key: str) -> str:
    """Read one top-level string field of `[project]` from pyproject.toml."""
    text = (REPO_ROOT / "pyproject.toml").read_text(encoding="utf-8")
    _, marker, remainder = text.partition("[project]")
    if not marker:
        raise RuntimeError("pyproject.toml has no [project] table")
    section = remainder.split("\n[", 1)[0]
    match = re.search(rf'^{key}\s*=\s*"([^"]*)"', section, re.MULTILINE)
    if match is None:
        raise RuntimeError(f"pyproject.toml [project] has no string `{key}`")
    return match.group(1)


def _version() -> str:
    return _project_field("version")


def _metadata() -> bytes:
    readme = (REPO_ROOT / _project_field("readme")).read_text(encoding="utf-8")
    return (
        "Metadata-Version: 2.1\n"
        f"Name: {_NAME}\n"
        f"Version: {_version()}\n"
        f"Summary: {_project_field('description')}\n"
        f"License: {_project_field('license')}\n"
        f"Requires-Python: {_project_field('requires-python')}\n"
        "Description-Content-Type: text/markdown\n"
        f"\n{readme}"
    ).encode("utf-8")


def _wheel_tag() -> str:
    interp = f"cp{sys.version_info.major}{sys.version_info.minor}"
    platform = sysconfig.get_platform().replace("-", "_").replace(".", "_")
    return f"{interp}-{interp}-{platform}"


def _dist_info() -> str:
    return f"{_NAME}-{_version()}.dist-info"


def _wheel_file() -> bytes:
    return (
        "Wheel-Version: 1.0\n"
        "Generator: fbuild-build-backend\n"
        "Root-Is-Purelib: false\n"
        f"Tag: {_wheel_tag()}\n"
    ).encode("utf-8")


class _WheelWriter:
    """Stored (uncompressed) wheel writer that tracks RECORD rows."""

    _EXEC_ATTR = (stat.S_IFREG | 0o755) << 16
    _FILE_ATTR = (stat.S_IFREG | 0o644) << 16

    def __init__(self, path: Path) -> None:
        self._zip = zipfile.ZipFile(path, "w", zipfile.ZIP_STORED)
        self._records: list[tuple[str, str, str]] = []

    def _info(self, arcname: str, executable: bool) -> zipfile.ZipInfo:
        info = zipfile.ZipInfo(arcname, date_time=(1980, 1, 1, 0, 0, 0))
        # Unix mode bits only count when create_system == 3 (see #129).
        info.create_system = 3
        info.external_attr = self._EXEC_ATTR if executable else self._FILE_ATTR
        return info

    def add_bytes(self, arcname: str, data: bytes, executable: bool = False) -> None:
        self._zip.writestr(self._info(arcname, executable), data)
        digest = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=")
        self._records.append((arcname, f"sha256={digest.decode()}", str(len(data))))

    def add_file(self, arcname: str, path: Path, executable: bool = False) -> None:
        """Stream a (possibly ~100 MB) file in, hashing as it is copied."""
        sha = hashlib.sha256()
        size = 0
        with (
            path.open("rb") as src,
            self._zip.open(
                self._info(arcname, executable), "w", force_zip64=True
            ) as dst,
        ):
            for chunk in iter(lambda: src.read(1 << 20), b""):
                sha.update(chunk)
                dst.write(chunk)
                size += len(chunk)
        digest = base64.urlsafe_b64encode(sha.digest()).rstrip(b"=").decode()
        self._records.append((arcname, f"sha256={digest}", str(size)))

    def close(self, dist_info: str) -> None:
        buf = io.StringIO()
        writer = csv.writer(buf, lineterminator="\n")
        writer.writerows(self._records)
        writer.writerow((f"{dist_info}/RECORD", "", ""))
        self._zip.writestr(self._info(f"{dist_info}/RECORD", False), buf.getvalue())
        self._zip.close()


def _write_wheel(wheel_directory: str, add_payload) -> str:
    _stage_artifacts()
    # Last line of defence: never wrap a missing or empty binary in a wheel.
    _assert_staged_payload()
    name = f"{_NAME}-{_version()}-{_wheel_tag()}.whl"
    writer = _WheelWriter(Path(wheel_directory) / name)
    # Raw native binaries go to .data/scripts/: pip copies them into bin/
    # (Scripts/ on Windows) verbatim, with no Python wrapper (#746).
    data = f"{_NAME}-{_version()}.data/scripts"
    writer.add_file(f"{data}/{TARGET_BINARY_NAME}", STAGED_BINARY_PATH, True)
    writer.add_file(f"{data}/{TARGET_DAEMON_NAME}", STAGED_DAEMON_PATH, True)
    add_payload(writer)
    dist_info = _dist_info()
    writer.add_bytes(f"{dist_info}/METADATA", _metadata())
    writer.add_bytes(f"{dist_info}/WHEEL", _wheel_file())
    writer.close(dist_info)
    return name


def _add_package(writer: _WheelWriter) -> None:
    package_root = PYTHON_EXT_DIR.parent
    for path in sorted(PYTHON_EXT_DIR.rglob("*.py")):
        if "__pycache__" not in path.parts:
            writer.add_file(path.relative_to(package_root).as_posix(), path)
    writer.add_file(f"{_NAME}/{STAGED_NATIVE_EXT_NAME}", STAGED_NATIVE_EXT_PATH, True)


def _add_editable_pth(writer: _WheelWriter) -> None:
    # A plain .pth onto python/, which every static analyzer resolves.
    writer.add_bytes(f"__editable__.{_NAME}.pth", f"{PYTHON_EXT_DIR.parent}\n".encode())


def get_requires_for_build_wheel(config_settings=None) -> list[str]:
    return []


def get_requires_for_build_editable(config_settings=None) -> list[str]:
    return []


def prepare_metadata_for_build_wheel(metadata_directory, config_settings=None) -> str:
    dist_info = Path(metadata_directory) / _dist_info()
    dist_info.mkdir(parents=True, exist_ok=True)
    (dist_info / "METADATA").write_bytes(_metadata())
    (dist_info / "WHEEL").write_bytes(_wheel_file())
    return dist_info.name


prepare_metadata_for_build_editable = prepare_metadata_for_build_wheel


def build_wheel(wheel_directory, config_settings=None, metadata_directory=None) -> str:
    return _write_wheel(wheel_directory, _add_package)


def build_editable(
    wheel_directory, config_settings=None, metadata_directory=None
) -> str:
    return _write_wheel(wheel_directory, _add_editable_pth)


# ---------------------------------------------------------------------------
# `python -m ci.build_backend --reclaim-stale-targets`
#
# The store is written by every `pip install` of every checkout, including
# agent worktrees, so the owner of the disk is rarely the process that notices.
# This entry point is the cron-shaped command `fbuild doctor` cannot be: it
# reclaims the store without a checkout, without building anything.
# ---------------------------------------------------------------------------


def main(argv: Optional["list[str]"] = None) -> int:
    parser = argparse.ArgumentParser(
        prog="python -m ci.build_backend",
        description=(
            "Reclaim dead ~/.fbuild/cargo-target/wheel-build targets "
            "(FastLED/fbuild#2062)."
        ),
    )
    parser.add_argument(
        "--reclaim-stale-targets",
        action="store_true",
        help="report and delete dead sibling targets (required)",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="report what would be reclaimed without deleting anything",
    )
    parser.add_argument(
        "--idle-days",
        type=float,
        default=None,
        help=(
            "idle gate for targets with no owner stamp "
            f"(default {DEFAULT_WHEEL_BUILD_IDLE_DAYS:g})"
        ),
    )
    parser.add_argument(
        "--store",
        type=Path,
        default=None,
        help="store to sweep (default: the real wheel-build store)",
    )
    args = parser.parse_args(sys.argv[1:] if argv is None else argv)
    if not args.reclaim_stale_targets:
        parser.error("pass --reclaim-stale-targets (this module builds wheels)")

    store = _wheel_build_store() if args.store is None else args.store
    reclaimed = reclaim_stale_wheel_build_targets(
        store, idle_days=args.idle_days, dry_run=args.dry_run
    )
    freed = sum(target.bytes_freed for target in reclaimed)
    verb = "would reclaim" if args.dry_run else "reclaimed"
    print(
        f"{store}: {verb} {_format_bytes(freed)} across {len(reclaimed)} dead target(s)"
    )
    for target in reclaimed:
        print(
            f"  {_format_bytes(target.bytes_freed)}  {target.path.name}  ({target.reason})"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
