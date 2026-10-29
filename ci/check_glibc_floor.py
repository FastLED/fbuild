"""Enforce the manylinux glibc floor on linked Linux binaries (#1629).

`ci/publish.py` assigns the `manylinux_2_17` wheel tag from a filename map, not
from the artifact, and `cargo check` never links -- so a wheel can promise
glibc >= 2.17 while needing a symbol from 2.27. That is exactly how
`libc::memfd_create` (glibc 2.27+) slipped past every PR gate and only failed in
the release cross-link lane.

The floor is a property of the *dynamic symbol table*, so this guard reads an
already-linked binary with `objdump -T` and fails when any `GLIBC_x.y` version
requirement is newer than the floor. It never builds anything; CI links the
release binaries first, then runs this over each artifact.

Usage::

    uv run --no-project python ci/check_glibc_floor.py BINARY [BINARY ...]
    uv run --no-project python ci/check_glibc_floor.py --floor 2.17 BINARY

Exit codes: 0 = at or below the floor, 1 = above the floor, 2 = the artifact
could not be inspected (missing file, no toolchain, not a glibc binary).
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path

DEFAULT_FLOOR = "2.17"
DEFAULT_OBJDUMP = "objdump"

_VERSION_RE = re.compile(r"^(\d+)\.(\d+)$")
# `objdump -T` prints the requirement in the version column:
#   0000000000000000      DF *UND*  0000000000000000  GLIBC_2.27 memfd_create
_SYMBOL_RE = re.compile(r"\bGLIBC_(\d+)\.(\d+)(?:\.(\d+))?\b")


@dataclass(frozen=True, order=True)
class Version:
    """A glibc symbol-version number, comparable field by field."""

    major: int
    minor: int
    patch: int

    @classmethod
    def parse(cls, text: str) -> Version:
        match = _VERSION_RE.match(text)
        if match is None:
            raise ValueError(f"expected MAJOR.MINOR, got {text!r}")
        return cls(int(match.group(1)), int(match.group(2)), 0)

    @classmethod
    def from_match(cls, match: re.Match[str]) -> Version:
        return cls(int(match.group(1)), int(match.group(2)), int(match.group(3) or 0))

    def __str__(self) -> str:
        base = f"{self.major}.{self.minor}"
        return base if self.patch == 0 else f"{base}.{self.patch}"


@dataclass(frozen=True)
class Finding:
    """One binary that needs a glibc symbol newer than the floor."""

    binary: Path
    version: Version
    symbols: tuple[str, ...]


def parse_requirements(symbol_table: str) -> dict[Version, set[str]]:
    """Map each required `GLIBC_x.y` version to the symbols that need it."""
    requirements: dict[Version, set[str]] = defaultdict(set)
    for line in symbol_table.splitlines():
        match = _SYMBOL_RE.search(line)
        if match is None:
            continue
        fields = line.split()
        symbol = fields[-1] if fields else "<unknown>"
        requirements[Version.from_match(match)].add(symbol)
    return dict(requirements)


def max_version(requirements: dict[Version, set[str]]) -> Version | None:
    return max(requirements) if requirements else None


def violations(
    binary: Path, requirements: dict[Version, set[str]], floor: Version
) -> list[Finding]:
    """Return one finding per too-new version, oldest first."""
    return [
        Finding(binary, version, tuple(sorted(requirements[version])))
        for version in sorted(requirements)
        if version > floor
    ]


def read_symbol_table(binary: Path, objdump: str) -> str:
    result = subprocess.run(
        [objdump, "-T", str(binary)],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(
            f"{objdump} -T {binary} failed ({result.returncode}): "
            f"{result.stderr.strip()}"
        )
    return result.stdout


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--floor",
        default=DEFAULT_FLOOR,
        help=f"highest permitted glibc symbol version (default {DEFAULT_FLOOR})",
    )
    parser.add_argument(
        "--objdump",
        default=DEFAULT_OBJDUMP,
        help=f"objdump executable to use (default {DEFAULT_OBJDUMP})",
    )
    parser.add_argument("binaries", nargs="+", help="linked ELF binaries to inspect")
    args = parser.parse_args(argv)

    try:
        floor = Version.parse(str(args.floor))
    except ValueError as error:
        parser.error(str(error))
        return 2  # unreachable; parser.error exits

    findings: list[Finding] = []
    failures: list[str] = []
    for name in args.binaries:
        binary = Path(str(name))
        if not binary.is_file():
            failures.append(f"{binary}: no such file")
            continue
        try:
            table = read_symbol_table(binary, str(args.objdump))
        except (OSError, RuntimeError) as error:
            failures.append(f"{error}")
            continue
        requirements = parse_requirements(table)
        newest = max_version(requirements)
        if newest is None:
            failures.append(f"{binary}: no GLIBC_ symbol versions found")
            continue
        status = "OK" if newest <= floor else "TOO NEW"
        print(
            f"glibc floor {floor}: {binary} (max GLIBC_{newest}) {status}", flush=True
        )
        findings.extend(violations(binary, requirements, floor))

    for finding in findings:
        print(
            f"error: {finding.binary} requires GLIBC_{finding.version} (floor {floor})",
            file=sys.stderr,
        )
        print(
            f"  GLIBC_{finding.version}: {', '.join(finding.symbols)}", file=sys.stderr
        )
    for failure in failures:
        print(f"error: {failure}", file=sys.stderr)

    if findings:
        print(
            "The manylinux_2_17 promise is broken by the symbols above; call the "
            "raw syscall (libc::syscall(libc::SYS_...)) or keep a fallback for "
            "glibc older than the floor.",
            file=sys.stderr,
        )
        return 1
    if failures:
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
