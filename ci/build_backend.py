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
"""

from __future__ import annotations

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
from pathlib import Path
from typing import Optional

REPO_ROOT = Path(__file__).resolve().parent.parent
TARGET_BINARY_NAME = "fbuild.exe" if sys.platform == "win32" else "fbuild"
TARGET_DAEMON_NAME = (
    "fbuild-daemon.exe" if sys.platform == "win32" else "fbuild-daemon"
)
STAGED_BIN_DIR = REPO_ROOT / "ci" / "bin"
STAGED_BINARY_PATH = STAGED_BIN_DIR / TARGET_BINARY_NAME
STAGED_DAEMON_PATH = STAGED_BIN_DIR / TARGET_DAEMON_NAME

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


def _staged_daemon_is_up_to_date() -> bool:
    """True if the staged daemon matches the current Rust workspace inputs."""
    if not STAGED_DAEMON_PATH.is_file():
        return False
    staged_mtime = STAGED_DAEMON_PATH.stat().st_mtime
    for path in _iter_cargo_inputs():
        try:
            if path.stat().st_mtime > staged_mtime:
                return False
        except FileNotFoundError:
            return False
    return True


def _staged_native_ext_is_up_to_date() -> bool:
    """True if the staged PyO3 extension exists and is newer than every cargo input.

    FastLED/fbuild#829: companion to `_staged_binary_is_up_to_date` for the
    Python extension. If either staged artifact is stale, both rebuild —
    they share the same cargo workspace and the same incremental cache.
    """
    if not STAGED_NATIVE_EXT_PATH.is_file():
        return False
    staged_mtime = STAGED_NATIVE_EXT_PATH.stat().st_mtime
    for path in _iter_cargo_inputs():
        try:
            if path.stat().st_mtime > staged_mtime:
                return False
        except FileNotFoundError:
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
            "the `ci/dev-tools` subpackage directly instead.\n"
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


def _find_fbuild_daemon_from_json(stdout: str) -> Optional[Path]:
    """Return the `fbuild-daemon` executable from cargo's artifact stream."""
    binary_path: Optional[Path] = None
    for line in stdout.splitlines():
        line = line.strip()
        if not line or not line.startswith("{"):
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        if msg.get("reason") != "compiler-artifact":
            continue
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
    for line in stdout.splitlines():
        line = line.strip()
        if not line or not line.startswith("{"):
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        if msg.get("reason") != "compiler-artifact":
            continue
        target = msg.get("target") or {}
        if target.get("name") != "_native":
            continue
        kinds = target.get("kind") or []
        if "cdylib" not in kinds:
            continue
        filenames = msg.get("filenames") or []
        for fn in filenames:
            p = Path(fn)
            # Pick the cdylib file (suffix varies by host):
            # - Windows: `.dll`
            # - Linux:   `.so`
            # - macOS:   `.dylib`
            if p.suffix in (".dll", ".so", ".dylib"):
                cdylib_path = p
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
    # Fast path: when every staged artifact is newer than every cargo input,
    # skip cargo entirely. Even cargo's "Fresh" pass walks the workspace and
    # takes wall-clock seconds.
    if (
        _staged_binary_is_up_to_date()
        and _staged_daemon_is_up_to_date()
        and _staged_native_ext_is_up_to_date()
    ):
        sys.stderr.write(
            f"  staged artifacts up-to-date "
            f"({STAGED_BINARY_PATH.name}, {STAGED_DAEMON_PATH.name}, "
            f"{STAGED_NATIVE_EXT_PATH.name}); skipping cargo\n"
        )
        return

    _require_soldr()
    binary_path, daemon_path = _build_fbuild_binaries()
    STAGED_BIN_DIR.mkdir(parents=True, exist_ok=True)
    shutil.copy2(binary_path, STAGED_BINARY_PATH)
    shutil.copy2(daemon_path, STAGED_DAEMON_PATH)
    # FastLED/fbuild#829: the PyO3 extension is staged alongside so
    # `from fbuild import ...` works after a source/editable install.
    cdylib_path = _build_fbuild_python()
    PYTHON_EXT_DIR.mkdir(parents=True, exist_ok=True)
    shutil.copy2(cdylib_path, STAGED_NATIVE_EXT_PATH)
    sys.stderr.write(f"  staged {STAGED_BIN_DIR} and {STAGED_NATIVE_EXT_PATH}\n")


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
    section = text.split("[project]", 1)[1].split("\n[", 1)[0]
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
        with path.open("rb") as src, self._zip.open(
            self._info(arcname, executable), "w", force_zip64=True
        ) as dst:
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


def build_editable(wheel_directory, config_settings=None, metadata_directory=None) -> str:
    return _write_wheel(wheel_directory, _add_editable_pth)
