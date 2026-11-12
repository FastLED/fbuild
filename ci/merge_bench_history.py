#!/usr/bin/env python3
"""Union two `history.jsonl` snapshots instead of letting the last publisher win.

`benchmark-build-comparison.yml` used to funnel every run -- the nightly cron and
every push to `main` -- through one `concurrency:` group. GitHub keeps at most
one *pending* run per group, so the 2026-11-12 nightly (run 43190818367) sat
queued behind a push run and was cancelled two seconds after the next merge
arrived: `cancel-in-progress: false` protects the RUNNING run, never the pending
one. The nightly therefore has its own group now (see
`ci/check_workflow_concurrency.py`), which means a nightly and a push run can
OVERLAP.

Overlap is harmless for everything else the publisher writes (`latest.json` and
the reports are simply the newest run's), but not for this file: each run reads
`history.jsonl` from the published branch when it STARTS, appends its own row,
and the publisher force-pushes the branch. Two overlapping runs publish two
snapshots taken at different times, so whichever lands last would revert the
other's row. The publisher merges instead:

  * identity is `(ts, sha)` -- republishing the same run cannot duplicate a row;
  * rows are emitted in chronological `ts` order with the newest last, matching
    `write_history` in `bench/fastled-examples/src/build_comparison.rs`;
  * at most `--max-lines` newest rows are kept (365, the same bound);
  * a line that is not a JSON object is passed through untouched -- it is
    published data, not ours to delete -- and is emitted before the sorted rows;
  * a row whose `ts` cannot be parsed sorts after every parsed row, so a broken
    timestamp can never push a real measurement out of the trimmed window.

Usage:

    python3 ci/merge_bench_history.py --published OLD --fresh NEW --output OUT

`--published` may be omitted or point at a missing file: `main` may not have a
published history yet.
"""

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path

# Same bound as `HISTORY_MAX_LINES` in bench/fastled-examples/src/build_comparison.rs.
MAX_LINES = 365
UNIX_PREFIX = "unix:"


@dataclass(frozen=True)
class Entry:
    """One parsed history row kept verbatim so the merge never reformats bytes."""

    row: dict[str, object]
    line: str


def timestamp_seconds(ts: object) -> float | None:
    """Seconds since the epoch for `unix:<s>` or ISO8601; None when unparseable."""
    if not isinstance(ts, str) or not ts:
        return None
    if ts.startswith(UNIX_PREFIX):
        try:
            return float(ts[len(UNIX_PREFIX) :])
        except ValueError:
            return None
    try:
        return datetime.fromisoformat(ts.replace("Z", "+00:00")).timestamp()
    except ValueError:
        return None


def identity(row: dict[str, object]) -> tuple[str, str]:
    """A run is (ts, sha): the same run published twice is one row."""
    ts = row.get("ts")
    sha = row.get("sha")
    return (
        ts if isinstance(ts, str) else "",
        sha if isinstance(sha, str) else "",
    )


def sort_key(entry: Entry) -> tuple[int, float, str]:
    seconds = timestamp_seconds(entry.row.get("ts"))
    if seconds is None:
        return (1, 0.0, entry.line)
    return (0, seconds, "")


def merge_history(published: str, fresh: str, max_lines: int = MAX_LINES) -> str:
    """Both snapshots as one chronological, de-duplicated, trimmed history."""
    passthrough: list[str] = []
    entries: dict[tuple[str, str], Entry] = {}
    for text in (published, fresh):
        for line in text.splitlines():
            if not line.strip():
                continue
            row = parse_object(line)
            if row is None:
                if line not in passthrough:
                    passthrough.append(line)
                continue
            # The fresh snapshot wins a (ts, sha) clash: it carries this run's row.
            entries[identity(row)] = Entry(row=row, line=line)

    ordered = sorted(entries.values(), key=sort_key)
    if max_lines > 0 and len(ordered) > max_lines:
        ordered = ordered[-max_lines:]
    return "".join(
        f"{line}\n" for line in passthrough + [entry.line for entry in ordered]
    )


def parse_object(line: str) -> dict[str, object] | None:
    try:
        value = json.loads(line)
    except json.JSONDecodeError:
        return None
    return value if isinstance(value, dict) else None


def read_optional(path: Path | None) -> str:
    if path is None or not path.is_file():
        return ""
    return path.read_text(encoding="utf-8")


def row_count(text: str) -> int:
    return sum(1 for line in text.splitlines() if line.strip())


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Merge two benchmark history snapshots."
    )
    parser.add_argument("--published", type=Path, default=None)
    parser.add_argument("--fresh", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--max-lines", type=int, default=MAX_LINES)
    args = parser.parse_args(argv)

    published = read_optional(args.published)
    fresh = read_optional(args.fresh)
    merged = merge_history(published, fresh, max_lines=args.max_lines)
    args.output.write_text(merged, encoding="utf-8")
    print(
        f"history: published={row_count(published)} fresh={row_count(fresh)} "
        f"merged={row_count(merged)} -> {args.output}",
        file=sys.stdout,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
