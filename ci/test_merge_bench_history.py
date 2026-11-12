"""Tests for the publisher's `history.jsonl` union (`ci/merge_bench_history.py`).

The nightly and a push benchmark can now overlap (they no longer share a
`concurrency:` group), so two publishers can land out of order. What must hold:
both runs' rows survive, duplicates collapse, order stays chronological, and the
365-line bound trims the OLDEST row, never the newest.
"""

from __future__ import annotations

import json
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

from ci import merge_bench_history as merge


def row(ts: str, sha: str, cold: float = 100.0) -> str:
    return json.dumps(
        {"results": [{"cold_ms": cold, "tool": "fbuild"}], "sha": sha, "ts": ts},
        separators=(",", ":"),
    )


def rows_of(text: str) -> list[dict]:
    """Parsed rows, skipping any line the merge deliberately passed through."""
    rows = []
    for line in text.splitlines():
        if not line.strip():
            continue
        try:
            rows.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    return rows


class MergeBenchHistoryTests(unittest.TestCase):
    def test_rows_from_both_snapshots_survive(self) -> None:
        """The older snapshot publishes later: its predecessor's row must not vanish."""
        published = row("2026-11-12T09:20:00Z", "aaaa", 10.0)
        fresh = row("2026-11-12T08:45:00Z", "bbbb", 20.0)

        merged = merge.merge_history(published + "\n", fresh + "\n")

        self.assertEqual(
            sorted(r["sha"] for r in rows_of(merged)),
            ["aaaa", "bbbb"],
        )

    def test_a_republished_run_is_not_duplicated(self) -> None:
        line = row("2026-11-12T09:20:00Z", "aaaa")

        merged = merge.merge_history(line + "\n", line + "\n")

        self.assertEqual(len(rows_of(merged)), 1)

    def test_chronological_order_with_newest_last(self) -> None:
        """`write_history` appends, so the last line is the newest run."""
        published = "\n".join(
            [
                row("2026-11-10T09:20:00Z", "ccc"),
                row("2026-11-12T09:20:00Z", "zzz"),
                row("2026-11-11T09:20:00Z", "mmm"),
            ]
        )

        merged = merge.merge_history(
            published + "\n", row("2026-11-13T09:20:00Z", "new") + "\n"
        )

        self.assertEqual(
            [r["ts"][:10] for r in rows_of(merged)],
            ["2026-11-10", "2026-11-11", "2026-11-12", "2026-11-13"],
        )

    def test_unix_timestamps_sort_with_iso_timestamps(self) -> None:
        """History carries both spellings (`unix:<s>` and ISO); order must be real time."""
        published = "\n".join(
            [
                row("unix:1700000000", "old"),  # 2023-11-14
                row("2026-11-12T09:20:00Z", "iso"),
            ]
        )

        merged = merge.merge_history(published + "\n", "")

        self.assertEqual([r["sha"] for r in rows_of(merged)], ["old", "iso"])

    def test_history_is_trimmed_to_the_newest_rows(self) -> None:
        published = "\n".join(
            row(f"2026-11-{day:02d}T09:20:00Z", f"s{day}") for day in range(1, 6)
        )
        fresh = row("2026-11-06T09:20:00Z", "newest")

        merged = merge.merge_history(published + "\n", fresh + "\n", max_lines=3)

        self.assertEqual([r["sha"] for r in rows_of(merged)], ["s4", "s5", "newest"])

    def test_a_malformed_line_is_kept_rather_than_dropped(self) -> None:
        """Published data is not ours to delete -- even a line we cannot parse."""
        published = "{not json\n" + row("2026-11-12T09:20:00Z", "aaaa") + "\n"

        merged = merge.merge_history(
            published, row("2026-11-13T09:20:00Z", "bbbb") + "\n"
        )

        self.assertIn("{not json", merged)
        self.assertEqual([r["sha"] for r in rows_of(merged)], ["aaaa", "bbbb"])

    def test_an_absent_published_file_keeps_the_fresh_snapshot(self) -> None:
        with TemporaryDirectory() as tmp:
            fresh = Path(tmp) / "fresh.jsonl"
            fresh.write_text(
                row("2026-11-12T09:20:00Z", "aaaa") + "\n", encoding="utf-8"
            )
            missing = Path(tmp) / "missing.jsonl"
            out = Path(tmp) / "out.jsonl"

            code = merge.main(
                [
                    "--published",
                    str(missing),
                    "--fresh",
                    str(fresh),
                    "--output",
                    str(out),
                ]
            )

            self.assertEqual(code, 0)
            self.assertEqual(
                [r["sha"] for r in rows_of(out.read_text(encoding="utf-8"))], ["aaaa"]
            )

    def test_cli_merges_both_files_and_reports_the_counts(self) -> None:
        with TemporaryDirectory() as tmp:
            published = Path(tmp) / "published.jsonl"
            published.write_text(
                row("2026-11-12T09:20:00Z", "aaaa") + "\n", encoding="utf-8"
            )
            fresh = Path(tmp) / "fresh.jsonl"
            fresh.write_text(
                row("2026-11-12T08:45:00Z", "bbbb") + "\n", encoding="utf-8"
            )
            out = Path(tmp) / "out.jsonl"

            code = merge.main(
                [
                    "--published",
                    str(published),
                    "--fresh",
                    str(fresh),
                    "--output",
                    str(out),
                ]
            )

            self.assertEqual(code, 0)
            self.assertEqual(len(rows_of(out.read_text(encoding="utf-8"))), 2)


if __name__ == "__main__":
    unittest.main()
