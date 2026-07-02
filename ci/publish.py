"""Wheel assembly for fbuild's PyPI release.

Library code consumed by the **Autonomous Release** GitHub Action
(`.github/workflows/release-auto.yml`). The action downloads
per-platform native binaries (Linux x86_64, Linux aarch64,
macOS aarch64, Windows x86_64) into ``dist/_tmp/binaries-<target>``,
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
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DIST_DIR = ROOT / "dist"
WHEEL_DIR = DIST_DIR / "wheels"
PYTHON_SHIMS_DIR = ROOT / "python"

# GitHub artifact name -> dist/ subdir
#
# Not every release-matrix lane maps to a wheel: the
# x86_64-apple-darwin and aarch64-pc-windows-msvc artifacts ship via
# the GitHub release archives only (Intel Macs install the ARM wheel
# via the dual macosx tag below; ARM Windows uses the win_amd64 wheel
# via emulation).
ARTIFACT_MAP: dict[str, str] = {
    "binaries-x86_64-unknown-linux-musl": "linux-x86_64",
    "binaries-aarch64-unknown-linux-musl": "linux-aarch64",
    # Restored alongside the release-auto.yml macOS matrix entries on
    # soldr v0.7.98 + setup-soldr v0.9.64 (Apple SDK URL fix).
    "binaries-aarch64-apple-darwin": "macos-aarch64",
    "binaries-x86_64-pc-windows-msvc": "windows-x86_64",
}

# dist/ subdir -> wheel platform tags
PLATFORMS: dict[str, list[str]] = {
    "linux-x86_64": ["manylinux_2_17_x86_64", "manylinux2014_x86_64"],
    "linux-aarch64": ["manylinux_2_17_aarch64", "manylinux2014_aarch64"],
    "macos-aarch64": ["macosx_11_0_arm64", "macosx_10_12_x86_64"],
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

def build_wheel(
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
