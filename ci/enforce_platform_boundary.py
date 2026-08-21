"""Enforce the transitional exact-occurrence host-platform ledger.

The checker walks every handwritten Rust source and crate manifest without
module expansion. It is independent of the compiler lint, so inactive,
private, orphaned, test, example, bench, and build-script code remains in the
cross-host union. Ordinary migration changes must remove source occurrences
and their ledger rows together.
"""

from __future__ import annotations

import argparse
import collections
import dataclasses
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
if str(REPO_ROOT) not in sys.path:
    sys.path.insert(0, str(REPO_ROOT))

from ci import platform_boundary_research as research  # noqa: E402

ROOT = research.ROOT
LEDGER = ROOT / "ci/platform_boundary_ledger.tsv"
DYLINT_BASELINE = ROOT / "dylints/enforce_platform_boundary/src/baseline.txt"
PLATFORM_ROOT = "crates/fbuild-core/src/platform/"
CONCRETE_PREFIXES = (
    *(PLATFORM_ROOT + host + "/" for host in ("windows", "linux", "macos")),
)
AUTHORIZED_BOUNDARY_FINDINGS = {
    (
        PLATFORM_ROOT + "executable.rs",
        "native_path",
        "std::env::current_exe",
        "host_executable",
        "host_mechanic",
    ),
    (
        "crates/fbuild-core/\x43argo.toml",
        "target_dependency_table",
        "[target.'cfg(windows)'.dependencies]",
        "fs",
        "host_mechanic",
    ),
    (
        "crates/fbuild-core/\x43argo.toml",
        "native_dependency",
        "windows-sys",
        "fs",
        "host_mechanic",
    ),
}
AUTHORIZED_BOUNDARY_FINDINGS.update(
    {
        (
            "crates/fbuild-core/\x43argo.toml",
            "target_dependency_table",
            "[target.'cfg(unix)'.dependencies]",
            "fs",
            "host_mechanic",
        ),
        (
            "crates/fbuild-core/\x43argo.toml",
            "native_dependency",
            "libc",
            "fs",
            "host_mechanic",
        ),
    }
)
AUTHORIZED_BOUNDARY_FINDINGS.update(
    {
        (
            "crates/fbuild-core/\x43argo.toml",
            "native_dependency",
            dependency,
            "ipc",
            "host_mechanic",
        )
        for dependency in ("interprocess", "socket2")
    }
)
LEDGER_KINDS = {
    "attr_cfg",
    "cfg_macro",
    "compile_host_fact",
    "concrete_module_ref",
    "native_dependency",
    "native_path",
    "target_dependency_table",
}
# Pre-expansion rustc visits exactly one of these mutually exclusive cfg
# bodies on any host, while the whole-tree scanner intentionally inventories
# both. Keep this projection adjustment explicit and occurrence-specific.
DYLINT_HOST_EXCLUSIVE_ADJUSTMENTS: collections.Counter[tuple[str, str, str]] = (
    collections.Counter()
)


@dataclasses.dataclass(frozen=True, order=True)
class LedgerRow:
    path: str
    kind: str
    normalized: str
    ordinal: int
    capability: str
    classification: str

    def tsv(self) -> str:
        return "\t".join(
            (
                self.path,
                self.kind,
                self.normalized,
                str(self.ordinal),
                self.capability,
                self.classification,
            )
        )


def rows_from_findings(findings: list[research.Finding]) -> list[LedgerRow]:
    """Convert line-oriented research findings into stable occurrence keys."""
    ordinals: collections.Counter[tuple[str, str, str]] = collections.Counter()
    rows: list[LedgerRow] = []
    for finding in findings:
        if finding.path.startswith(CONCRETE_PREFIXES) or (
            finding.path,
            finding.kind,
            finding.normalized,
            finding.capability,
            finding.classification,
        ) in AUTHORIZED_BOUNDARY_FINDINGS:
            continue
        key = (finding.path, finding.kind, finding.normalized)
        ordinal = ordinals[key]
        ordinals[key] += 1
        rows.append(
            LedgerRow(
                *key,
                ordinal,
                finding.capability,
                finding.classification,
            )
        )
    return sorted(rows)


def render(rows: list[LedgerRow]) -> str:
    header = "path\tkind\tnormalized\tordinal\tcapability\tclassification"
    return header + "\n" + "\n".join(row.tsv() for row in rows) + "\n"


def parse_ledger(path: Path = LEDGER) -> list[LedgerRow]:
    rows: list[LedgerRow] = []
    for line_number, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if line_number == 1 and raw.startswith("path\t"):
            continue
        if not raw or raw.startswith("#"):
            continue
        fields = raw.split("\t")
        if len(fields) != 6:
            raise ValueError(f"{path}:{line_number}: expected six tab-separated fields")
        source, kind, normalized, ordinal_text, capability, classification = fields
        if kind not in LEDGER_KINDS:
            raise ValueError(f"{path}:{line_number}: unknown kind {kind!r}")
        try:
            ordinal = int(ordinal_text)
        except ValueError as error:
            raise ValueError(f"{path}:{line_number}: ordinal must be an integer") from error
        if ordinal < 0:
            raise ValueError(f"{path}:{line_number}: ordinal must not be negative")
        rows.append(LedgerRow(source, kind, normalized, ordinal, capability, classification))
    return rows


def validate_ledger(rows: list[LedgerRow]) -> list[str]:
    """Reject malformed, duplicate, stale-path, and allowed-zone rows."""
    failures: list[str] = []
    if rows != sorted(rows):
        failures.append("ledger rows are not sorted")
    if len(rows) != len(set(rows)):
        failures.append("ledger contains duplicate rows")
    sources = {path.relative_to(ROOT).as_posix() for path in research.source_files(ROOT)}
    sources.update(path.relative_to(ROOT).as_posix() for path in (ROOT / "crates").glob("*/Cargo.toml"))
    grouped: dict[tuple[str, str, str], list[int]] = collections.defaultdict(list)
    for row in rows:
        grouped[(row.path, row.kind, row.normalized)].append(row.ordinal)
        if row.path not in sources:
            failures.append(f"stale or out-of-scope row: {row.path}")
        if row.path.startswith(CONCRETE_PREFIXES):
            failures.append(f"private implementation row must not be baselined: {row.path}")
    for key, ordinals in sorted(grouped.items()):
        if sorted(ordinals) != list(range(len(ordinals))):
            failures.append(f"non-contiguous ordinals for {' '.join(key)}: {ordinals}")
    return failures


def compare(expected: list[LedgerRow], observed: list[LedgerRow]) -> list[str]:
    failures: list[str] = []
    expected_set = set(expected)
    observed_set = set(observed)
    for row in sorted(observed_set - expected_set):
        failures.append(f"new occurrence: {row.tsv()}")
    for row in sorted(expected_set - observed_set):
        failures.append(f"stale occurrence: {row.tsv()}")
    return failures


def parse_dylint_baseline(path: Path = DYLINT_BASELINE) -> collections.Counter[tuple[str, str, str]]:
    """Read the exact counts consumed by the pre-expansion Dylint."""
    rows: collections.Counter[tuple[str, str, str]] = collections.Counter()
    for line_number, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if not raw or raw.startswith("#"):
            continue
        fields = raw.split("\t")
        if len(fields) != 4:
            raise ValueError(f"{path}:{line_number}: expected four tab-separated fields")
        source, kind, normalized, ordinal_text = fields
        ordinal = int(ordinal_text)
        if ordinal != rows[(source, kind, normalized)]:
            raise ValueError(f"{path}:{line_number}: duplicate or non-contiguous ordinal")
        rows[(source, kind, normalized)] += 1
    return rows


def scanner_dylint_counts(rows: list[LedgerRow]) -> collections.Counter[tuple[str, str, str]]:
    """Project the whole-tree ledger onto the Dylint's syntax categories."""
    counts: collections.Counter[tuple[str, str, str]] = collections.Counter()
    for row in rows:
        if row.path.startswith(CONCRETE_PREFIXES):
            continue
        if row.kind in {"attr_cfg", "cfg_macro"}:
            identifiers = research.IDENTIFIER.findall(row.normalized)
            for identifier in identifiers:
                if identifier in research.HOST_KEYS:
                    counts[(row.path, row.kind, identifier)] += 1
        elif row.kind == "native_path":
            normalized = row.normalized
            if normalized == "std::env::current_exe":
                key = normalized
            elif normalized.startswith("std::os::"):
                parts = normalized.split("::")
                key = "::".join(parts[:3])
            else:
                key = normalized.split("::", 1)[0]
            counts[(row.path, "native_import", key)] += 1
        elif row.kind == "compile_host_fact":
            facts = [identifier for identifier in research.IDENTIFIER.findall(row.normalized) if identifier.startswith("CARGO_CFG_TARGET_")]
            key = facts[0] if facts else row.normalized
            counts[(row.path, row.kind, key)] += 1
        elif row.kind == "concrete_module_ref":
            counts[(row.path, "module_ref", row.normalized)] += 1
    for key, adjustment in DYLINT_HOST_EXCLUSIVE_ADJUSTMENTS.items():
        if counts[key] < adjustment:
            raise ValueError(f"invalid Dylint host-exclusive adjustment for {key}")
        counts[key] -= adjustment
        if counts[key] == 0:
            del counts[key]
    return counts


def render_dylint_baseline(counts: collections.Counter[tuple[str, str, str]]) -> str:
    lines = ["# path<TAB>kind<TAB>normalized<TAB>ordinal"]
    for key, count in sorted(counts.items()):
        lines.extend("\t".join((*key, str(ordinal))) for ordinal in range(count))
    return "\n".join(lines) + "\n"


def parse_dylint_observations(
    path: Path,
) -> tuple[
    set[tuple[str, str]],
    collections.Counter[tuple[str, str, str, str]],
]:
    """Read observations emitted by actual pre-expansion lint processes."""
    sources: set[tuple[str, str]] = set()
    findings: collections.Counter[tuple[str, str, str, str]] = collections.Counter()
    for line_number, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        fields = raw.split("\t")
        if len(fields) != 4:
            raise ValueError(f"{path}:{line_number}: expected four tab-separated fields")
        process, source, kind, normalized = fields
        if kind == "source_seen":
            sources.add((process, source))
        else:
            findings[(process, source, kind, normalized)] += 1
    return sources, findings


def compare_dylint_observations(
    expected: collections.Counter[tuple[str, str, str]],
    sources: set[tuple[str, str]],
    findings: collections.Counter[tuple[str, str, str, str]],
) -> list[str]:
    """Require exact Dylint coverage for every source a driver compiled."""
    failures: list[str] = []
    if not sources:
        return ["actual Dylint observation file contains no compiled sources"]
    expected_by_path: dict[str, collections.Counter[tuple[str, str]]] = collections.defaultdict(collections.Counter)
    for (source, kind, normalized), count in expected.items():
        expected_by_path[source][(kind, normalized)] = count
    for process, source in sorted(sources):
        actual = collections.Counter({(kind, normalized): count for (owner, path, kind, normalized), count in findings.items() if owner == process and path == source})
        wanted = expected_by_path.get(source, collections.Counter())
        if actual != wanted:
            failures.append(f"actual Dylint observations disagree for pid={process} source={source}: expected={dict(wanted)} actual={dict(actual)}")
    return failures


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write", action="store_true")
    parser.add_argument("--print-totals", action="store_true")
    parser.add_argument("--dylint-observed", type=Path)
    args = parser.parse_args(argv)
    observed = rows_from_findings(research.inventory(ROOT))
    if args.write:
        LEDGER.write_text(render(observed), encoding="utf-8", newline="\n")
        DYLINT_BASELINE.write_text(
            render_dylint_baseline(scanner_dylint_counts(observed)),
            encoding="utf-8",
            newline="\n",
        )
    try:
        expected = parse_ledger()
        dylint_expected = parse_dylint_baseline()
    except (OSError, ValueError) as error:
        print(f"platform-boundary: {error}", file=sys.stderr)
        return 1
    failures = [*validate_ledger(expected), *compare(expected, observed)]
    dylint_observed = scanner_dylint_counts(observed)
    if dylint_expected != dylint_observed:
        failures.append("Dylint baseline and independent scanner disagree")
    if args.dylint_observed is not None:
        try:
            sources, findings = parse_dylint_observations(args.dylint_observed)
        except (OSError, ValueError) as error:
            failures.append(f"cannot read actual Dylint observations: {error}")
        else:
            failures.extend(compare_dylint_observations(dylint_observed, sources, findings))
    if args.print_totals:
        print(f"rows={len(expected)}; dylint_rows={sum(dylint_expected.values())}")
    for failure in failures:
        print(f"platform-boundary: {failure}", file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
