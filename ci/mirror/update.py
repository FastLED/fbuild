#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["boto3>=1.36", "httpx>=0.27"]
# ///
"""Incremental mirror pass: mirror only registry releases the bucket lacks.

The full pass (`rotate.py`) needs droplets because the catalog is ~100 GB and
the CDN caps each IP at ~36 GB a day. New releases are a few files a day, so
this runs from one machine, with no droplet (FastLED/fbuild#2040 item 4):

1. Download the bucket's `_state/manifest.db` and keep the ETag it was read at.
2. Catalog. By default, page the registry search newest-updated first down to
   the DB's watermark (`--since` once, when the DB has none), re-read the
   versions of every package updated since, and queue each file the DB lacks.
   `--package owner/type/name` refreshes only the named packages and leaves the
   watermark alone (the FastLED 3.10.9 case, FastLED/fbuild#2034).
3. Print the queue: every pending/error row, exactly what step 5 transfers.
   `--dry-run` stops here and writes nothing to the bucket.
4. Refuse a queue bigger than `--max-bytes`.
5. Copy the bucket DB to `_state/manifest.db.bak-<UTC>` (only if it is still
   the copy read), transfer the queue, then upload the DB with If-Match on the
   ETag read in step 1. The DB is uploaded even when the transfer stopped
   early, so files that did reach the bucket are recorded.
6. Regenerate the index (`ci/gen_mirror_manifest.py`), check it, and commit
   `site/mirror/`. This script prints the commands; it does not touch git.

Exit status: 0 done, 1 refused by a guard (nothing written), 3 the registry
refused the client (`mirror.RegistryRefused`).

    uv run --no-project --script ci/mirror/update.py --dry-run
    uv run --no-project --script ci/mirror/update.py --since 2026-10-01 --dry-run
    uv run --no-project --script ci/mirror/update.py --package fastled/library/FastLED
"""

from __future__ import annotations

import argparse
import asyncio
import os
import sqlite3
import sys
from datetime import datetime, timezone
from pathlib import Path

import mirror  # sibling scripts: ci/mirror/
import rotate

WORK = rotate.LOCAL_DIR / "update-work"
DEFAULT_MAX_BYTES = 5 * 10**9


class GuardError(RuntimeError):
    """A precondition failed; nothing was written to the bucket."""


def parse_since(value: str) -> int:
    """`YYYY-MM-DD` or an ISO datetime (UTC unless it says otherwise) -> epoch."""
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as exc:
        raise argparse.ArgumentTypeError(f"not an ISO date: {value!r}") from exc
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return int(parsed.timestamp())


def parse_package(value: str) -> tuple[str, str, str]:
    parts = value.split("/")
    if len(parts) != 3 or not all(parts):
        raise argparse.ArgumentTypeError(f"expected owner/type/name, got {value!r}")
    return parts[0], parts[1], parts[2]


def queued(db_path: Path) -> list[tuple[str, int]]:
    """`(key, size)` of every row `mirror.transfer` would pick up."""
    conn = sqlite3.connect(db_path)
    try:
        return conn.execute(
            "select key, coalesce(size, 0) from files"
            " where state in ('pending','error') order by key"
        ).fetchall()
    finally:
        conn.close()


def check_scan_source(db_path: Path, args: argparse.Namespace) -> None:
    """A search scan needs a starting point: the stored watermark or `--since`."""
    if args.package or args.since is not None:
        return
    conn = sqlite3.connect(db_path)
    try:
        conn.executescript(mirror.SCHEMA)
        watermark = mirror.stored_watermark(conn)
    finally:
        conn.close()
    if watermark is None:
        raise GuardError(
            "the state DB records no watermark, so it cannot tell which known"
            " packages have new releases; pass --since <date> once (the start of"
            " the last full pass, or earlier) or --package"
        )


def check_queue(rows: list[tuple[str, int]], max_bytes: int) -> None:
    total = sum(size for _, size in rows)
    if total > max_bytes:
        raise GuardError(
            f"{len(rows)} files, {total} bytes queued: over --max-bytes {max_bytes};"
            " raise it deliberately or use rotate.py for a pass this large"
        )


def snapshot_db(db_path: Path) -> Path:
    """A self-contained copy of the live (WAL) DB to upload."""
    out = db_path.with_name("manifest.upload.db")
    out.unlink(missing_ok=True)
    src = sqlite3.connect(db_path)
    dst = sqlite3.connect(out)
    try:
        src.backup(dst)
    finally:
        dst.close()
        src.close()
    return out


def print_queue(rows: list[tuple[str, int]]) -> None:
    for key, size in rows:
        print(f"  {size:>12}  {key}")
    print(f"queued: {len(rows)} files, {sum(s for _, s in rows)} bytes")


def fresh_work_db() -> Path:
    WORK.mkdir(parents=True, exist_ok=True)
    db_path = WORK / "manifest.db"
    for leftover in ("manifest.db", "manifest.db-wal", "manifest.db-shm"):
        (WORK / leftover).unlink(missing_ok=True)
    return db_path


def publish(db_path: Path, read: rotate.BucketCopy, user_agent: str) -> int:
    """Back up the bucket DB, transfer the queue, upload the DB (step 5)."""
    backup = rotate.backup_bucket_manifest(read)
    mirror.log(f"backed up the bucket state DB to {backup}")
    try:
        status = asyncio.run(mirror.transfer(user_agent))
    finally:
        # Record whatever reached the bucket, even if the transfer died.
        upload = snapshot_db(db_path)
        rotate.put_bucket_manifest(upload, read)
        mirror.log(f"uploaded {rotate.STATE_KEY} ({rotate.summarize_manifest(upload)})")
    return status


def parse_args(argv: list[str] | None) -> argparse.Namespace:
    assert __doc__ is not None
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--since", type=parse_since, help="scan from this date")
    ap.add_argument(
        "--package",
        type=parse_package,
        action="append",
        default=[],
        help="owner/type/name to refresh instead of scanning (repeatable)",
    )
    ap.add_argument("--dry-run", action="store_true", help="list; write nothing")
    ap.add_argument("--max-bytes", type=int, default=DEFAULT_MAX_BYTES)
    return ap.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        user_agent = mirror.resolve_user_agent(os.environ)
    except ValueError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    mirror.configure(WORK, rotate.CREDS)
    db_path = fresh_work_db()
    read = rotate.download_bucket_manifest(db_path)
    if read is None:
        print("error: the bucket has no state DB; run rotate.py", file=sys.stderr)
        return 1
    mirror.log(f"bucket state DB: {read.summary}, ETag {read.etag}")
    try:
        check_scan_source(db_path, args)
        asyncio.run(mirror.catalog(user_agent, since=args.since, packages=args.package))
        rows = queued(db_path)
        print_queue(rows)
        if args.dry_run:
            print("dry run: nothing was written to the bucket")
            return 0
        if not rows and args.package:
            print("nothing new; the bucket is unchanged")
            return 0
        check_queue(rows, args.max_bytes)
    except GuardError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    except mirror.RegistryRefused as exc:
        mirror.record_refusal(exc)
        return mirror.EXIT_REFUSED
    status = publish(db_path, read, user_agent)
    print(
        "next: uv run --no-project --with boto3 python ci/gen_mirror_manifest.py"
        f" --credentials {rotate.CREDS} (then --check) and commit site/mirror/"
    )
    return status


if __name__ == "__main__":
    sys.exit(main())
