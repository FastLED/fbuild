# /// script
# requires-python = ">=3.11"
# dependencies = ["zstandard>=0.23"]
# ///
"""Slice an extracted ESP toolchain into one self-contained archive per chip.

`uv run --no-project ci/slice_toolchain.py <toolchain-dir> --out-dir OUT`

The unified Xtensa/RISC-V toolchains ship SDK libraries for every chip they
support (`xtensa-esp-elf/lib/esp32`, `.../esp32s2`, `.../esp32s3`). A build for
one chip needs its own directory and nothing else, so a per-chip archive drops
the siblings and compresses what remains: FastLED/fbuild#1944 measured 660 MB extracted
-> 75.3 MB for esp32s3 at zstd -19, against a 168 MB vendor download.

Selection is by path component, not by a hardcoded list: any directory whose
name looks like a chip (`esp32s3`, `esp32c6`, ...) is chip-specific, and a slice
keeps only the requested chip's. Everything else -- `bin`, `libexec`, `include`,
the top-level multilib archives -- is kept, because dropping a shared file
breaks the link (the SAM "built but no `main`" class).

Two directions are refused rather than guessed: a requested chip with no
directory of its own (nothing to slice), and a tree with no per-chip
directories at all (a whole-tree copy would be an empty promise). `--verify`
extracts each slice and runs that chip's compiler from the extraction, so a
slice is only reported usable after a real compile.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
from dataclasses import dataclass
from pathlib import Path, PurePosixPath

import zstandard

CHIP = re.compile(r"^esp32[a-z0-9]*$")
SOURCE_DATE = 0  # fixed mtime: identical input must give identical bytes
# fbuild marks a finished install with this file; a slice must never carry
# one, or an interrupted extraction would pass as a complete install.
INSTALL_MARKER = ".install_complete"
MIN_LEVEL = 1
MAX_LEVEL = 19


def digest(path: Path) -> dict:
    with path.open("rb") as stream:
        sha = hashlib.file_digest(stream, "sha256").hexdigest()
    return {"sha256": sha, "size_bytes": path.stat().st_size}


@dataclass(frozen=True)
class Entry:
    """One member of the source tree, relative to the toolchain root."""

    rel: str
    kind: str  # "file" | "dir" | "link"
    mode: int
    target: str = ""


def _kind(path: Path) -> tuple[str, str, int]:
    info = path.lstat()
    mode = stat.S_IMODE(info.st_mode)
    if stat.S_ISLNK(info.st_mode):
        return "link", os.readlink(path), mode
    if stat.S_ISDIR(info.st_mode):
        return "dir", "", mode
    if stat.S_ISREG(info.st_mode):
        return "file", "", mode
    raise ValueError(f"unsupported file type: {path}")


def _entry(root: Path, rel: str) -> Entry:
    kind, target, mode = _kind(root / rel)
    return Entry(rel, kind, mode, target)


def scan(root: Path) -> list[Entry]:
    """Every member under `root`, sorted, without following symlinks.

    The install marker is dropped: see `INSTALL_MARKER`.
    """
    if not root.is_dir():
        raise ValueError(f"not a directory: {root}")
    entries: list[Entry] = []
    for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
        dirnames.sort()
        filenames.sort()
        parent = Path(dirpath).relative_to(root).as_posix()
        names = dirnames + filenames
        prefix = "" if parent == "." else parent + "/"
        wanted = [name for name in names if name != INSTALL_MARKER]
        entries.extend(_entry(root, prefix + name) for name in wanted)
    return sorted(entries, key=lambda e: e.rel)


def parts_of(entry: Entry) -> tuple[str, ...]:
    return PurePosixPath(entry.rel).parts


def discover_chips(entries: list[Entry]) -> tuple[str, ...]:
    """Chip names that appear as a path component, e.g. `lib/esp32s3`."""
    found = {part for entry in entries for part in parts_of(entry) if CHIP.match(part)}
    return tuple(sorted(found))


def _contained(parent: tuple[str, ...], target: PurePosixPath) -> bool:
    """True when a relative link target never climbs above the root.

    Legitimate toolchains use in-tree `..` links (`lib/bfd-plugins/
    liblto_plugin.so -> ../../libexec/...`), so only escapes are refused.
    """
    depth = len(parent)
    for part in target.parts:
        if part == "..":
            depth -= 1
            if depth < 0:
                return False
        elif part != ".":
            depth += 1
    return True


def _safe_link(entry: Entry) -> None:
    if entry.kind != "link":
        return
    target = PurePosixPath(entry.target)
    if target.is_absolute() or not _contained(parts_of(entry)[:-1], target):
        raise ValueError(f"unsafe symlink: {entry.rel} -> {entry.target}")


def select(entries: list[Entry], chip: str, chips: tuple[str, ...]) -> list[Entry]:
    """Keep the requested chip's directories plus every chip-shared member."""
    if chip not in chips:
        raise ValueError(f"no {chip} directory in toolchain (have: {', '.join(chips)})")
    others = set(chips) - {chip}
    kept = [
        entry
        for entry in entries
        if chip in parts_of(entry) or not others.intersection(parts_of(entry))
    ]
    foreign = {part for entry in kept for part in parts_of(entry)} & others
    if foreign:
        raise ValueError(f"{chip} slice carries foreign chip dirs: {sorted(foreign)}")
    if not kept:
        raise ValueError(f"{chip} slice is empty")
    return kept


def _resolve_link(entry: Entry) -> str:
    """The in-root path a relative symlink resolves to, normalized."""
    stack = list(parts_of(entry)[:-1])
    for part in PurePosixPath(entry.target).parts:
        if part == ".":
            continue
        if part == "..":
            stack.pop()
        else:
            stack.append(part)
    return "/".join(stack)


def check_links(entries: list[Entry]) -> None:
    """Refuse a slice whose in-tree symlinks point at dropped members."""
    kept = {entry.rel for entry in entries}
    broken = sorted(
        f"{entry.rel} -> {entry.target}"
        for entry in entries
        if entry.kind == "link"
        and not PurePosixPath(entry.target).is_absolute()
        and _contained(parts_of(entry)[:-1], PurePosixPath(entry.target))
        and _resolve_link(entry) not in kept
    )
    if broken:
        raise ValueError(f"slice drops symlink targets: {broken}")


def _info(entry: Entry, size: int) -> tarfile.TarInfo:
    info = tarfile.TarInfo(entry.rel)
    info.mtime = SOURCE_DATE
    info.uid = 0
    info.gid = 0
    info.uname = ""
    info.gname = ""
    info.mode = entry.mode
    if entry.kind == "dir":
        info.type = tarfile.DIRTYPE
    elif entry.kind == "link":
        info.type = tarfile.SYMTYPE
        info.linkname = entry.target
    else:
        info.type = tarfile.REGTYPE
        info.size = size
    return info


def write_slice(path: Path, root: Path, entries: list[Entry], level: int) -> dict:
    """Write one deterministic `tar.zst`; return its digest and payload size."""
    if not MIN_LEVEL <= level <= MAX_LEVEL:
        raise ValueError(f"compression level must be {MIN_LEVEL}..{MAX_LEVEL}")
    raw_bytes = 0
    with path.open("wb") as raw:
        compressor = zstandard.ZstdCompressor(
            level=level, write_checksum=True, threads=0
        )
        with compressor.stream_writer(raw) as compressed:
            with tarfile.open(
                fileobj=compressed, mode="w|", format=tarfile.PAX_FORMAT
            ) as out:
                for entry in entries:
                    _safe_link(entry)
                    if entry.kind != "file":
                        out.addfile(_info(entry, 0))
                        continue
                    source = root / entry.rel
                    size = source.stat().st_size
                    raw_bytes += size
                    with source.open("rb") as data:
                        out.addfile(_info(entry, size), data)
    return {
        "name": path.name,
        **digest(path),
        "uncompressed_bytes": raw_bytes,
        "entries": len(entries),
    }


def _extract(archive: Path, dest: Path) -> None:
    dest.mkdir(parents=True, exist_ok=True)
    with archive.open("rb") as raw, zstandard.ZstdDecompressor().stream_reader(
        raw
    ) as stream:
        with tarfile.open(fileobj=stream, mode="r|") as tar:
            if sys.version_info >= (3, 12):
                tar.extractall(dest, filter="data")
            else:  # pragma: no cover - 3.10/3.11 runners
                tar.extractall(dest)


def _compiler(root: Path, chip: str) -> Path:
    """The chip's own `*-gcc` when the toolchain ships one, else the generic."""
    binaries = sorted(p for p in (root / "bin").glob("*-gcc") if p.is_file())
    if not binaries:
        raise ValueError(f"no compiler in slice: {(root / 'bin')}")
    matching = [p for p in binaries if chip in p.name]
    return matching[0] if matching else binaries[0]


def _run(argv: list[str]) -> subprocess.CompletedProcess:
    return subprocess.run(argv, capture_output=True, text=True, check=False)


def verify_slice(archive: Path, chip: str, scratch: Path) -> dict:
    """Extract the slice and prove its compiler runs and links from it."""
    dest = scratch / f"verify-{chip}"
    _extract(archive, dest)
    compiler = _compiler(dest, chip)
    version = _run([str(compiler), "--version"])
    if version.returncode != 0:
        raise ValueError(f"{chip} compiler failed: {version.stderr.strip()}")
    libgcc = _run([str(compiler), "-print-libgcc-file-name"])
    if libgcc.returncode != 0:
        raise ValueError(f"{chip} cannot report libgcc: {libgcc.stderr.strip()}")
    resolved = Path(libgcc.stdout.strip()).resolve()
    if not resolved.is_relative_to(dest.resolve()):
        raise ValueError(f"{chip} libgcc resolves outside the slice: {resolved}")
    unit = scratch / f"{chip}-probe.cpp"
    unit.write_text("int probe() { return 1; }\n")
    compiled = _run(
        [str(compiler), "-std=gnu++17", "-Os", "-c", str(unit), "-o", str(scratch / f"{chip}-probe.o")]
    )
    if compiled.returncode != 0:
        raise ValueError(f"{chip} probe compile failed: {compiled.stderr.strip()}")
    return {
        "compiler": compiler.name,
        "version": version.stdout.splitlines()[0],
        "libgcc": resolved.relative_to(dest.resolve()).as_posix(),
        "compiled": unit.name,
    }


def _stem(root: Path) -> tuple[str, str]:
    """Package name/version from `package.json`, else the directory name."""
    manifest = root / "package.json"
    if manifest.is_file():
        data = json.loads(manifest.read_text())
        name, version = data.get("name"), data.get("version")
        if isinstance(name, str) and isinstance(version, str):
            return name, version
    return root.name, ""


def _token(text: str, field: str) -> str:
    cleaned = re.sub(r"[^0-9A-Za-z._+-]", "_", text)
    if not cleaned:
        raise ValueError(f"invalid {field}")
    return cleaned


def slice_toolchain(
    root: Path,
    out_dir: Path,
    chips: tuple[str, ...] = (),
    *,
    level: int = MAX_LEVEL,
    verify: bool = False,
    check: bool = False,
    prefix: str = "slices/",
) -> dict:
    entries = scan(root)
    available = discover_chips(entries)
    if not available:
        raise ValueError("toolchain has no per-chip directories; nothing to slice")
    wanted = chips or available
    unknown = [chip for chip in wanted if chip not in available]
    if unknown:
        raise ValueError(
            f"no {', '.join(unknown)} directory in toolchain "
            f"(have: {', '.join(available)})"
        )
    name, version = _stem(root)
    label = _token(name, "toolchain name") + (f"-{_token(version, 'version')}" if version else "")
    result = {
        "toolchain": name,
        "version": version,
        "chips_available": list(available),
        "slices": [],
    }
    with tempfile.TemporaryDirectory(prefix="toolchain-slices-") as tmp:
        scratch = Path(tmp)
        for chip in wanted:
            archive = scratch / f"{label}-{chip}.tar.zst"
            chosen = select(entries, chip, available)
            check_links(chosen)
            record = write_slice(archive, root, chosen, level)
            record["chip"] = chip
            if verify:
                record["verified"] = verify_slice(archive, chip, scratch)
            result["slices"].append(record)
        result["manifest_slices"] = {
            record["chip"]: {
                "url": prefix + record["name"],
                "sha256": record["sha256"],
                "size": record["size_bytes"],
                "format": "tar.zst",
            }
            for record in result["slices"]
        }
        publish_or_check(out_dir, scratch, result, check)
    return result


def publish_or_check(output: Path, scratch: Path, result: dict, check: bool) -> None:
    if check:
        stored = json.loads((output / "slices.json").read_text())
        if stored != result:
            raise ValueError("slice index differs")
        for record in result["slices"]:
            if digest(output / record["name"]) != {
                key: record[key] for key in ("sha256", "size_bytes")
            }:
                raise ValueError(f"slice differs: {record['name']}")
        return
    output.mkdir(parents=True, exist_ok=True)
    for record in result["slices"]:
        shutil.copyfile(scratch / record["name"], output / record["name"])
    (output / "slices.json").write_text(json.dumps(result, indent=2) + "\n")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path, help="extracted toolchain root")
    parser.add_argument("--out-dir", type=Path, required=True)
    parser.add_argument("--chips", default="", help="comma-separated, default: all")
    parser.add_argument("--level", type=int, default=MAX_LEVEL)
    parser.add_argument("--verify", action="store_true")
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--slice-prefix", default="slices/")
    args = parser.parse_args()
    chips = tuple(c.strip() for c in args.chips.split(",") if c.strip())
    result = slice_toolchain(
        args.source,
        args.out_dir,
        chips,
        level=args.level,
        verify=args.verify,
        check=args.check,
        prefix=args.slice_prefix,
    )
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
