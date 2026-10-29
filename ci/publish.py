"""Wheel assembly for fbuild's PyPI release.

Library code consumed by the **Autonomous Release** GitHub Action
(`.github/workflows/release-auto.yml`). The action downloads
per-platform native binaries (Linux x86_64, Linux aarch64,
macOS x86_64, macOS aarch64, Windows x86_64) into ``dist/_tmp/binaries-<target>``,
then calls into this module to:

1. Re-stage each artifact into ``dist/<platform_subdir>/`` (the
   layout :func:`build_wheel` expects).
2. Read project metadata (name, version, summary, requires-python,
   long description) from ``pyproject.toml``.
3. Assemble one wheel per platform in :data:`WHEEL_DIR`.

The wheels are then uploaded to PyPI by the action's
``publish-pypi`` job via PyPI trusted publishing (OIDC) — there is
no longer a local ``./publish`` script. To cut a release, bump
``Cargo.toml`` + ``pyproject.toml`` to the new version, push to
``main``, and the workflow handles building + tagging +
publishing. See ``docs/RELEASING.md``.

The module intentionally exposes only the library surface the
workflow imports:

- :data:`DIST_DIR`, :data:`WHEEL_DIR`, :data:`PYTHON_SHIMS_DIR`
- :data:`ARTIFACT_MAP`, :data:`PLATFORMS`, :data:`EXTENSION_NAMES`
- :func:`log`, :func:`record_hash`
- :func:`read_project_meta`
- :func:`build_wheel`, :func:`build_all_wheels`
- :data:`PYPI_PROJECT_SIZE_LIMIT_BYTES`, :class:`PyPIStorage`
- :func:`estimate_upload_storage`, :func:`measured_upload_storage`
"""

from __future__ import annotations

import base64
import csv
import hashlib
import io
import shutil
import stat
import sys
import tomllib
import zipfile
from collections.abc import Iterable
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DIST_DIR = ROOT / "dist"
WHEEL_DIR = DIST_DIR / "wheels"
PYTHON_SHIMS_DIR = ROOT / "python"

# GitHub artifact name -> dist/ subdir
#
# Not every release-matrix lane maps to a wheel: the
# aarch64-pc-windows-msvc artifact ships via the GitHub release archives
# only (ARM Windows uses the win_amd64 wheel via emulation).
# Each macOS wheel contains the matching native extension and CLI binaries.
ARTIFACT_MAP: dict[str, str] = {
    "binaries-x86_64-unknown-linux-musl": "linux-x86_64",
    "binaries-aarch64-unknown-linux-musl": "linux-aarch64",
    # Restored alongside the release-auto.yml macOS matrix entries once
    # Built with the repository-wide published soldr v0.8.8 pin.
    "binaries-aarch64-apple-darwin": "macos-aarch64",
    "binaries-x86_64-apple-darwin": "macos-x86_64",
    "binaries-x86_64-pc-windows-msvc": "windows-x86_64",
}

# dist/ subdir -> wheel platform tags
PLATFORMS: dict[str, list[str]] = {
    "linux-x86_64": ["manylinux_2_17_x86_64", "manylinux2014_x86_64"],
    "linux-aarch64": ["manylinux_2_17_aarch64", "manylinux2014_aarch64"],
    "macos-aarch64": ["macosx_11_0_arm64"],
    "macos-x86_64": ["macosx_10_12_x86_64"],
    "windows-x86_64": ["win_amd64"],
}

# Extension filenames produced by build.yml
EXTENSION_NAMES = {"_native.abi3.so", "_native.pyd"}


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def log(msg: str) -> None:
    print(msg, file=sys.stderr, flush=True)


def read_project_meta() -> tuple[str, str, str, str, str]:
    """Return (name, version, summary, requires_python, readme) from pyproject.toml."""
    with open(ROOT / "pyproject.toml", "rb") as f:
        data = tomllib.load(f)
    proj = data["project"]
    readme = ""
    readme_field = proj.get("readme")
    if readme_field:
        readme_path = ROOT / (readme_field if isinstance(readme_field, str) else readme_field.get("file", ""))
        if readme_path.exists():
            readme = readme_path.read_text(encoding="utf-8")
    return (
        proj["name"],
        proj["version"],
        proj.get("description", ""),
        proj.get("requires-python", ">=3.10"),
        readme,
    )


def record_hash(data: bytes) -> str:
    digest = hashlib.sha256(data).digest()
    return "sha256=" + base64.urlsafe_b64encode(digest).rstrip(b"=").decode()


# ---------------------------------------------------------------------------
# Wheel build
# ---------------------------------------------------------------------------

def build_wheel(  # noqa: C901
    name: str,
    version: str,
    summary: str,
    requires_python: str,
    readme: str,
    platform_subdir: str,
    plat_tags: list[str],
) -> Path | None:
    bin_dir = DIST_DIR / platform_subdir
    if not bin_dir.exists():
        return None

    # Separate CLI binaries from PyO3 extension
    cli_binaries: list[Path] = []
    extension_file: Path | None = None
    for f in sorted(bin_dir.iterdir()):
        if f.name in EXTENSION_NAMES:
            extension_file = f
        else:
            cli_binaries.append(f)

    if not cli_binaries:
        return None

    has_extension = extension_file is not None
    name_norm = name.replace("-", "_")
    tag_plat = ".".join(plat_tags)
    data_dir = f"{name_norm}-{version}.data"
    dist_info = f"{name_norm}-{version}.dist-info"

    # abi3 tag when extension is present, generic py3 otherwise
    if has_extension:
        tag_prefix = "cp310-abi3"
        wheel_filename = f"{name_norm}-{version}-cp310-abi3-{tag_plat}.whl"
    else:
        tag_prefix = "py3-none"
        wheel_filename = f"{name_norm}-{version}-py3-none-{tag_plat}.whl"

    metadata = (
        f"Metadata-Version: 2.1\n"
        f"Name: {name}\n"
        f"Version: {version}\n"
        f"Summary: {summary}\n"
        f"Requires-Python: {requires_python}\n"
    )
    if readme:
        # Per PEP 566: blank line separates headers from the description body,
        # and Description-Content-Type declares the rendering format PyPI uses.
        metadata += f"Description-Content-Type: text/markdown\n\n{readme}\n"

    wheel_meta = (
        f"Wheel-Version: 1.0\n"
        f"Generator: fbuild-publish\n"
        f"Root-Is-Purelib: false\n"
    )
    for pt in plat_tags:
        wheel_meta += f"Tag: {tag_prefix}-{pt}\n"

    # S_IFREG is required — pip's wheel installer calls S_ISREG() on the
    # upper 16 bits of external_attr and falls back to the umask default
    # (0o644) if the file-type bit is missing, regardless of the mode
    # bits set. That's how 2.1.20 shipped with mode=0o755 but still
    # `/bin/fbuild: Permission denied` on every Linux/macOS install.
    # Reference: uv/ruff wheels have external_attr 0x81ed0000
    # (S_IFREG | 0o755); 2.1.20 had 0x01ed0000.
    exec_perms = (
        stat.S_IFREG
        | stat.S_IRUSR | stat.S_IWUSR | stat.S_IXUSR
        | stat.S_IRGRP | stat.S_IXGRP
        | stat.S_IROTH | stat.S_IXOTH
    )

    WHEEL_DIR.mkdir(parents=True, exist_ok=True)
    wheel_path = WHEEL_DIR / wheel_filename
    record_rows: list[tuple[str, str, int]] = []

    def add_file(whl: zipfile.ZipFile, arcname: str, data: bytes, executable: bool = False) -> None:
        info = zipfile.ZipInfo(arcname)
        info.compress_type = zipfile.ZIP_DEFLATED
        if executable:
            # Unix permission bits live in the upper 16 of external_attr,
            # BUT only when create_system == 3 (Unix). The Python
            # `ZipInfo` default is 0 (DOS/Windows), under which
            # external_attr encodes DOS file-attribute flags instead,
            # and every unpacker (pip, installer, unzip) then ignores
            # the Unix mode and installs the file without +x. That's
            # what caused `fbuild --version` → Permission denied after
            # `pip install fbuild==2.1.18` — see FastLED/fbuild#129.
            info.create_system = 3
            info.external_attr = exec_perms << 16
        whl.writestr(info, data)
        record_rows.append((arcname, record_hash(data), len(data)))

    with zipfile.ZipFile(wheel_path, "w", zipfile.ZIP_DEFLATED) as whl:
        # CLI binaries → .data/scripts/
        for binary in cli_binaries:
            add_file(whl, f"{data_dir}/scripts/{binary.name}", binary.read_bytes(), executable=True)

        # Python shims + extension → fbuild/ package
        if has_extension:
            # Add Python shim files from python/fbuild/
            for shim in sorted(PYTHON_SHIMS_DIR.rglob("*.py")):
                rel = shim.relative_to(PYTHON_SHIMS_DIR)
                add_file(whl, str(rel).replace("\\", "/"), shim.read_bytes())

            # Add compiled extension into fbuild/ package
            add_file(
                whl,
                f"{name_norm}/{extension_file.name}",
                extension_file.read_bytes(),
                executable=True,
            )

        # dist-info
        meta_bytes = metadata.encode()
        add_file(whl, f"{dist_info}/METADATA", meta_bytes)

        wheel_bytes = wheel_meta.encode()
        add_file(whl, f"{dist_info}/WHEEL", wheel_bytes)

        buf = io.StringIO()
        writer = csv.writer(buf, lineterminator="\n")
        for row in record_rows:
            writer.writerow(row)
        writer.writerow((f"{dist_info}/RECORD", "", ""))
        whl.writestr(f"{dist_info}/RECORD", buf.getvalue().encode())

    size_mb = wheel_path.stat().st_size / (1024 * 1024)
    ext_label = " +ext" if has_extension else " (cli-only)"
    log(f"  {wheel_filename} ({size_mb:.1f} MB){ext_label}")
    return wheel_path


def build_all_wheels(name: str, version: str, summary: str, requires_python: str, readme: str) -> list[Path]:
    log(f"\n=== Build wheels ({name} {version}) ===")

    if WHEEL_DIR.exists():
        shutil.rmtree(WHEEL_DIR)

    wheels: list[Path] = []
    missing: list[str] = []
    for subdir, plat_tags in PLATFORMS.items():
        whl = build_wheel(name, version, summary, requires_python, readme, subdir, plat_tags)
        if whl:
            wheels.append(whl)
        else:
            missing.append(subdir)

    if missing:
        # PyPI does not allow re-uploading the same filename, so a partial
        # release would strand the version. Fail fast and let the action
        # mark the release as failed.
        log(f"  ERROR: failed to build wheels for: {', '.join(missing)}")
        log(f"  Refusing to publish a partial release.")
        sys.exit(1)

    log(f"  {len(wheels)} wheel(s) ready")
    return wheels


# ---------------------------------------------------------------------------
# PyPI storage quota preflight
# ---------------------------------------------------------------------------

# PyPI refuses uploads once a project's stored bytes reach its project storage
# limit, and it refuses them *mid-upload*: the first files of a release land and
# the rest 400 out ("400 Project size too large"). fbuild v2.5.46 stranded that
# way at 1 of 5 wheels, which leaves the project claiming a version nobody can
# install (FastLED/fbuild#1756). Both release stages therefore check headroom
# before a single file is uploaded.
#
# https://docs.pypi.org/project-management/storage-limits/
PYPI_PROJECT_SIZE_LIMIT_BYTES = 10 * 1000**3  # PyPI's "10 GB", counted decimally
PYPI_STORAGE_LIMITS_URL = "https://docs.pypi.org/project-management/storage-limits/"


def _file_bytes(entry: dict) -> int:
    """Stored size of one PyPI JSON file entry; 0 when the field is absent."""
    try:
        return int(entry.get("size") or 0)
    except (TypeError, ValueError):
        return 0


def pypi_project_bytes(pypi_json: dict) -> int:
    """Total stored bytes across every file of every version."""
    releases = pypi_json.get("releases") or {}
    return sum(_file_bytes(entry) for files in releases.values() for entry in files)


def pypi_version_sizes(pypi_json: dict, version: str) -> list[int]:
    """Stored byte sizes of the files already published for ``version``."""
    releases = pypi_json.get("releases") or {}
    return [_file_bytes(entry) for entry in releases.get(version) or []]


def _all_file_sizes(pypi_json: dict) -> list[int]:
    releases = pypi_json.get("releases") or {}
    return [_file_bytes(entry) for files in releases.values() for entry in files]


@dataclass(frozen=True)
class PyPIStorage:
    """Headroom verdict for one upload batch."""

    project_bytes: int
    required_bytes: int
    limit_bytes: int = PYPI_PROJECT_SIZE_LIMIT_BYTES
    basis: str = ""

    @property
    def remaining_bytes(self) -> int:
        return max(self.limit_bytes - self.project_bytes, 0)

    @property
    def fits(self) -> bool:
        return self.project_bytes + self.required_bytes <= self.limit_bytes

    def report(self) -> str:
        used_gb = self.project_bytes / 1e9
        limit_gb = self.limit_bytes / 1e9
        head = (
            f"PyPI storage: {used_gb:.2f} GB of a {limit_gb:.2f} GB project limit "
            f"({self.remaining_bytes / 1e9:.2f} GB free)"
        )
        need = f"this upload needs {self.required_bytes / 1e6:.0f} MB{self.basis}"
        if self.fits:
            return f"{head}\n{need} - headroom is sufficient"
        shortfall = (self.project_bytes + self.required_bytes - self.limit_bytes) / 1e6
        return "\n".join(
            [
                head,
                need,
                f"refusing to publish: this would exceed the limit by {shortfall:.0f} MB, and PyPI",
                "enforces the limit mid-upload, which strands a version at partial wheels (#1756).",
                "Free space first: request a project size increase or delete old releases in the",
                f"PyPI web UI - {PYPI_STORAGE_LIMITS_URL}",
            ]
        )


def estimate_upload_storage(
    pypi_json: dict,
    version: str,
    expected_wheels: int,
    limit_bytes: int = PYPI_PROJECT_SIZE_LIMIT_BYTES,
) -> PyPIStorage:
    """Preflight for completing ``version``: estimate the missing wheels' weight.

    The wheels are not built yet at this point, so the per-wheel weight is taken
    from what is already stored - this version's own files when some landed (the
    stranded-release case), otherwise the largest wheel of any previous release,
    which errs on the refusing side.
    """
    project = pypi_project_bytes(pypi_json)
    sizes = pypi_version_sizes(pypi_json, version)
    missing = max(expected_wheels - len(sizes), 0)
    if sizes:
        per_wheel, source = max(sizes), "this version's uploaded files"
    else:
        others = _all_file_sizes(pypi_json)
        per_wheel, source = (max(others) if others else 0), "the largest stored file"
    basis = ""
    if missing:
        basis = (
            f" for {missing} of {expected_wheels} missing wheel(s), at {per_wheel / 1e6:.0f} MB each"
            f" estimated from {source}"
        )
    return PyPIStorage(project, missing * per_wheel, limit_bytes, basis)


def measured_upload_storage(
    pypi_json: dict,
    wheels: Iterable[Path],
    limit_bytes: int = PYPI_PROJECT_SIZE_LIMIT_BYTES,
) -> PyPIStorage:
    """Preflight with the real wheel sizes, after the wheels are built.

    Wheels already on PyPI are counted again (``skip-existing`` would skip them),
    so this check is deliberately conservative.
    """
    files = list(wheels)
    project = pypi_project_bytes(pypi_json)
    required = sum(path.stat().st_size for path in files)
    return PyPIStorage(project, required, limit_bytes, f" for {len(files)} built wheel(s)")


def main(argv: list[str] | None = None) -> int:
    """CLI used by the Autonomous Release workflow to gate publication."""
    import argparse
    import json

    parser = argparse.ArgumentParser(description="PyPI storage headroom preflight.")
    parser.add_argument("--pypi-json", type=Path, required=True, help="saved /pypi/<name>/json")
    parser.add_argument("--version", default="", help="version being completed")
    parser.add_argument("--expected-wheels", type=int, default=0)
    parser.add_argument("--wheel", type=Path, action="append", default=[], help="built wheel")
    parser.add_argument("--limit-bytes", type=int, default=PYPI_PROJECT_SIZE_LIMIT_BYTES)
    args = parser.parse_args(argv)

    data = json.loads(args.pypi_json.read_text(encoding="utf-8"))
    if args.wheel:
        storage = measured_upload_storage(data, args.wheel, args.limit_bytes)
    else:
        storage = estimate_upload_storage(
            data, args.version, args.expected_wheels, args.limit_bytes
        )
    print(storage.report(), flush=True)
    return 0 if storage.fits else 1


if __name__ == "__main__":
    raise SystemExit(main())
