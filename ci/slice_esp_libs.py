# /// script
# requires-python = ">=3.11"
# dependencies = ["zstandard>=0.23"]
# ///
"""Stream an ESP libs archive once, then emit deterministic self-contained slices.

uv run --no-project ci/slice_esp_libs.py SOURCE --version 3.3.5 --out-dir OUTPUT
Payloads are spooled to temporary disk to sort arbitrary upstream tar ordering.
The JSON index is written to stdout and OUTPUT/slices.json; --check never writes OUTPUT.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import stat
import tarfile
import tempfile
import urllib.request
import zipfile
from pathlib import Path, PurePosixPath

import zstandard


def digest(path: Path) -> dict:
    with path.open("rb") as stream:
        sha = hashlib.file_digest(stream, "sha256").hexdigest()
    return {"sha256": sha, "size_bytes": path.stat().st_size}


def safe_name(name: str) -> str:
    path = PurePosixPath(name)
    if not path.parts:
        raise ValueError("empty archive path")
    if path.is_absolute() or ".." in path.parts or "\\" in name or ":" in path.parts[0]:
        raise ValueError(f"unsafe archive path: {name}")
    return str(path)


def _record(members: dict, name: str, member, payload: Path) -> None:
    if name in members:
        raise ValueError(f"duplicate archive path: {name}")
    if not (member.isfile() or member.isdir() or member.issym()):
        raise ValueError(f"unsupported archive member: {name}")
    if member.issym():
        target = PurePosixPath(safe_name(member.linkname))
        if target.is_absolute() or ".." in target.parts:
            raise ValueError(f"unsafe symlink: {name}")
    members[name] = (member, payload)


def _check_ancestors(members: dict) -> None:
    for name in members:
        for parent in PurePosixPath(name).parents:
            if str(parent) in members and members[str(parent)][0].issym():
                raise ValueError(f"symlink ancestor: {name}")


def spool_tar(source: Path, scratch: Path) -> dict:
    """Read the xz once; never extract paths supplied by the archive."""
    members: dict = {}
    with tarfile.open(source, "r|xz") as archive:
        for member in archive:
            name = safe_name(member.name)
            payload = scratch / str(len(members))
            if member.isfile():
                with archive.extractfile(member) as src, payload.open("wb") as dst:
                    shutil.copyfileobj(src, dst)
            _record(members, name, member, payload)
    _check_ancestors(members)
    return members


def spool_zip(source: Path, scratch: Path) -> dict:
    """Read a ZIP once; normalize members to ``TarInfo`` for the slicer."""
    members: dict = {}
    with zipfile.ZipFile(source) as archive:
        for info in archive.infolist():
            name = safe_name(info.filename)
            mode = (info.external_attr >> 16) & 0xFFFF
            member = tarfile.TarInfo(name)
            member.mode = (mode & 0o777) or 0o644
            payload = scratch / str(len(members))
            if info.is_dir():
                member.type = tarfile.DIRTYPE
                member.size = 0
            elif stat.S_ISLNK(mode):
                member.type = tarfile.SYMTYPE
                member.linkname = archive.read(info).decode()
                member.size = 0
            else:
                member.type = tarfile.REGTYPE
                member.size = info.file_size
                with archive.open(info) as src, payload.open("wb") as dst:
                    shutil.copyfileobj(src, dst)
            _record(members, name, member, payload)
    _check_ancestors(members)
    return members


def spool(source: Path, scratch: Path) -> dict:
    """Dispatch on magic bytes: ``PK`` is a ZIP, ``\\xfd7zXZ`` an xz tarball."""
    with source.open("rb") as handle:
        magic = handle.read(6)
    if magic[:2] == b"PK":
        return spool_zip(source, scratch)
    if magic[:6] == b"\xfd7zXZ\x00":
        return spool_tar(source, scratch)
    raise ValueError(f"unsupported archive format: {magic!r}")


def write_slice(path: Path, names: list[str], members: dict, level: int) -> dict:
    total = 0
    with path.open("wb") as raw:
        compressor = zstandard.ZstdCompressor(
            level=level, write_checksum=True, threads=0
        )
        with compressor.stream_writer(raw) as compressed:
            with tarfile.open(
                fileobj=compressed, mode="w|", format=tarfile.PAX_FORMAT
            ) as out:
                for name in names:
                    original, payload = members[name]
                    member = tarfile.TarInfo(name)
                    member.type = original.type
                    member.linkname = original.linkname
                    member.mode = original.mode & 0o777
                    member.size = original.size if original.isfile() else 0
                    total += member.size
                    if original.isfile():
                        with payload.open("rb") as data:
                            out.addfile(member, data)
                    else:
                        out.addfile(member)
    return {
        "name": path.name,
        **digest(path),
        "uncompressed_bytes": total,
        "entries": len(names),
    }


def publish_or_check(output: Path, scratch: Path, result: dict, check: bool) -> None:
    if check:
        if json.loads((output / "slices.json").read_text()) != result:
            raise ValueError("slice index differs")
        for record in result["slices"]:
            if digest(output / record["name"]) != {
                k: record[k] for k in ("sha256", "size_bytes")
            }:
                raise ValueError(f"slice differs: {record['name']}")
    else:
        output.mkdir(parents=True, exist_ok=True)
        for record in result["slices"]:
            shutil.copyfile(scratch / record["name"], output / record["name"])
        (output / "slices.json").write_text(json.dumps(result, indent=2) + "\n")


def slice_archive(
    source: str, version: str, output: Path, *, check: bool = False, level: int = 19
) -> dict:
    if not 1 <= level <= 19:
        raise ValueError("compression level must be between 1 and 19")
    if not version or any(
        c not in "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ.-"
        for c in version
    ):
        raise ValueError("invalid version")
    with tempfile.TemporaryDirectory(prefix="esp-slices-") as tmp:
        scratch = Path(tmp)
        if source.startswith(("https://", "http://")):
            archive = scratch / PurePosixPath(source.split("?")[0]).name
            with urllib.request.urlopen(source) as src, archive.open("wb") as dst:
                shutil.copyfileobj(src, dst)
        else:
            archive = Path(source)
        source_info = digest(archive)
        members = spool(archive, scratch)
        roots = {PurePosixPath(n).parts[0] for n in members}
        if roots != {"esp32-arduino-libs"}:
            raise ValueError(f"unexpected archive roots: {sorted(roots)}")
        chips = sorted(
            {
                PurePosixPath(n).parts[1]
                for n in members
                if (len(PurePosixPath(n).parts) > 2 or members[n][0].isdir())
                and len(PurePosixPath(n).parts) > 1
                and PurePosixPath(n).parts[1].startswith("esp32")
            }
        )
        if not chips:
            raise ValueError("no chip directories")
        result = {"source": source_info, "slices": []}
        for chip in chips:
            names = sorted(
                n
                for n in members
                if len(PurePosixPath(n).parts) == 1
                or PurePosixPath(n).parts[1] not in chips
                or PurePosixPath(n).parts[1] == chip
            )
            name = f"esp32-arduino-libs-{version}-{chip}.tar.zst"
            record = write_slice(scratch / name, names, members, level)
            record["chip"] = chip
            result["slices"].append(record)
        publish_or_check(output, scratch, result, check)
        return result


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source")
    parser.add_argument("--version", required=True)
    parser.add_argument("--out-dir", type=Path, required=True)
    parser.add_argument("--level", type=int, choices=range(1, 20), default=19)
    parser.add_argument(
        "--slice-prefix",
        default="slices/",
        help="object key prefix relative to the manifest base",
    )
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    result = slice_archive(
        args.source, args.version, args.out_dir, check=args.check, level=args.level
    )
    result["manifest_slices"] = {
        r["chip"]: {
            "url": args.slice_prefix + r["name"],
            "sha256": r["sha256"],
            "size": r["size_bytes"],
            "format": "tar.zst",
        }
        for r in result["slices"]
    }
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
