#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["httpx>=0.27", "boto3>=1.34"]
# ///
"""fbuild open-source package mirror.

Mirrors every file in the PlatformIO registry catalog into the public
DigitalOcean Spaces bucket `fbuild` (sfo3), preserving the registry download
path as the object key:

    https://dl.registry.platformio.org/download/platformio/tool/framework-arduino-avr/5.4.0/framework-arduino-avr-5.4.0.tar.gz
    -> fbuild:sfo3/ platformio/tool/framework-arduino-avr/5.4.0/framework-arduino-avr-5.4.0.tar.gz

Public reads work anonymously (`fbuild.sfo3.digitaloceanspaces.com`), so an
fbuild client can use it as a download mirror without credentials.

Subcommands
-----------
catalog   Enumerate the registry (all packages, all versions, all files) into
          the local SQLite manifest. Resumable and idempotent.
transfer  Download every pending file from the registry, verify sha256, upload
          it to Spaces with a public-read ACL, mark it done. Resumable; safe to
          re-run after a crash or reboot.
run       catalog then transfer, forever (re-catalogs periodically to pick up
          new registry versions).
status    Print manifest progress by state.
verify    Re-HEAD a sample (or all) of the done rows against Spaces.

The registry's CDN rejects requests whose User-Agent does not start with
`PlatformIO/` (HTTP 403 "This download method is not supported"), so every
registry request sends FBUILD_USER_AGENT below.
"""

from __future__ import annotations

import argparse
import asyncio
import hashlib
import json
import os
import shutil
import sqlite3
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

# boto3/httpx are imported where they are used, so the pure helpers can be
# imported (and unit tested) without the script's dependencies installed.

API = "https://api.registry.platformio.org/v3"
DL_PREFIX = "https://dl.registry.platformio.org/download/"
# The CDN's daily download quota is keyed on the exact User-Agent string (a fresh
# droplet IP is capped instantly when the string is the same), so allow an override.
USER_AGENT = os.environ.get(
    "FBUILD_MIRROR_UA",
    "PlatformIO/6.1.16 (fbuild-mirror; +https://github.com/FastLED/fbuild)",
)

STATE_DIR = Path(os.environ.get("FBUILD_MIRROR_STATE", "/var/lib/fbuild-mirror"))
DB_PATH = STATE_DIR / "manifest.db"
CREDS_PATH = Path(
    os.environ.get("FBUILD_MIRROR_CREDS", "/etc/fbuild-mirror/spaces.json")
)
BUCKET = os.environ.get("FBUILD_MIRROR_BUCKET", "fbuild")
REGION = os.environ.get("FBUILD_MIRROR_REGION", "sfo3")
CONCURRENCY = int(os.environ.get("FBUILD_MIRROR_CONCURRENCY", "3"))
RECATALOG_SECONDS = int(
    os.environ.get("FBUILD_MIRROR_RECATALOG_SECONDS", str(12 * 3600))
)

SCHEMA = """
create table if not exists packages (
  owner text not null,
  type text not null,
  name text not null,
  tier text,
  seen_at integer not null,
  versions_fetched integer not null default 0,
  primary key (owner, type, name)
);
create table if not exists files (
  url text primary key,
  owner text not null,
  type text not null,
  package text not null,
  version text not null,
  filename text not null,
  system text,
  size integer,
  sha256 text,
  key text not null,
  state text not null default 'pending',
  attempts integer not null default 0,
  last_error text,
  updated_at integer
);
create index if not exists files_state on files(state);
create index if not exists files_package on files(owner, type, package);
"""


def db() -> sqlite3.Connection:
    STATE_DIR.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(DB_PATH, timeout=60)
    conn.execute("pragma journal_mode=wal")
    conn.executescript(SCHEMA)
    return conn


def s3_client() -> Any:
    import boto3
    from botocore.client import Config

    creds = json.loads(CREDS_PATH.read_text())
    return boto3.client(
        "s3",
        region_name=REGION,
        endpoint_url=f"https://{REGION}.digitaloceanspaces.com",
        aws_access_key_id=creds["access_key"],
        aws_secret_access_key=creds["secret_key"],
        config=Config(
            retries={"max_attempts": 5, "mode": "standard"}, max_pool_connections=32
        ),
    )


def system_label(value: Any) -> str | None:
    """Registry `system` is either "*"/a string or a list of system tags."""
    if value is None:
        return None
    if isinstance(value, str):
        return value
    return json.dumps(value, sort_keys=True)


def object_key(url: str) -> str:
    if url.startswith(DL_PREFIX):
        return url[len(DL_PREFIX) :]
    # Non-registry-hosted files keep a stable, collision-free key.
    return "external/" + url.split("://", 1)[1]


def log(msg: str) -> None:
    print(f"{time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())} {msg}", flush=True)


# --------------------------------------------------------------------------
# catalog
# --------------------------------------------------------------------------


async def _search_page(client: Any, page: int) -> Any:
    """One `/search` page, retried; the registry is flaky."""
    for attempt in range(6):
        try:
            r = await client.get(
                f"{API}/search", params={"query": "*", "page": page, "limit": 50}
            )
            r.raise_for_status()
            return r
        except Exception as exc:  # registry is flaky
            log(f"search page {page} attempt {attempt + 1} failed: {exc}")
            await asyncio.sleep(2 * (attempt + 1))
    raise SystemExit(f"search page {page} kept failing; stopping (resumable)")


def _record_packages(
    conn: sqlite3.Connection, items: list[dict[str, Any]], now: int
) -> None:
    with conn:
        for it in items:
            owner = (it.get("owner") or {}).get("username") or "?"
            conn.execute(
                "insert or replace into packages"
                " (owner, type, name, tier, seen_at, versions_fetched)"
                " values (?,?,?,?,?, coalesce((select versions_fetched from packages"
                "  where owner=? and type=? and name=?), 0))",
                (
                    owner,
                    it.get("type"),
                    it.get("name"),
                    it.get("tier"),
                    now,
                    owner,
                    it.get("type"),
                    it.get("name"),
                ),
            )


async def _search_catalog(
    client: Any, conn: sqlite3.Connection, now: int, log_every: int
) -> None:
    """Page through the whole registry search into the `packages` table."""
    page = 1
    seen = 0
    while True:
        data = (await _search_page(client, page)).json()
        total = data["total"]
        items = data["items"]
        if not items:
            break
        _record_packages(conn, items, now)
        seen += len(items)
        if seen % log_every < len(items):
            log(f"catalog search {seen}/{total}")
        if seen >= total:
            break
        page += 1
        await asyncio.sleep(0.05)


def _mark_fetched(conn: sqlite3.Connection, owner: str, kind: str, name: str) -> None:
    with conn:
        conn.execute(
            "update packages set versions_fetched=1 where owner=? and type=? and name=?",
            (owner, kind, name),
        )


async def _get_versions(client: Any, url: str, label: str) -> tuple[str, Any]:
    """`("ok", body)`, `("gone", None)` for 404/410, or `("failed", None)`."""
    for attempt in range(4):
        try:
            rr = await client.get(url)
            if rr.status_code in (404, 410):
                return "gone", None
            rr.raise_for_status()
            return "ok", rr.json()
        except Exception as exc:
            if attempt == 3:
                log(f"versions {label} failed: {exc}")
                return "failed", None
            await asyncio.sleep(1.5 * (attempt + 1))
    return "failed", None


def file_rows(
    body: dict[str, Any], owner: str, kind: str, name: str, now: int
) -> list[tuple[Any, ...]]:
    """`files` rows for every downloadable file of every version in `body`."""
    rows = []
    for ver in body.get("versions") or []:
        for f in ver.get("files") or []:
            dl = f.get("download_url")
            if not dl:
                continue
            rows.append(
                (
                    dl,
                    owner,
                    kind,
                    name,
                    ver.get("name"),
                    f.get("name") or dl.rsplit("/", 1)[-1],
                    system_label(f.get("system")),
                    f.get("size") or 0,
                    (f.get("checksum") or {}).get("sha256"),
                    object_key(dl),
                    now,
                )
            )
    return rows


def _store_files(
    conn: sqlite3.Connection,
    rows: list[tuple[Any, ...]],
    owner: str,
    kind: str,
    name: str,
) -> None:
    try:
        with conn:
            conn.executemany(
                "insert or ignore into files"
                " (url, owner, type, package, version, filename, system, size,"
                "  sha256, key, state, updated_at)"
                " values (?,?,?,?,?,?,?,?,?,?,'pending',?)",
                rows,
            )
            conn.execute(
                "update packages set versions_fetched=1 where owner=? and type=? and name=?",
                (owner, kind, name),
            )
    except Exception as exc:  # one poison package must not stop the pass
        log(f"catalog metadata error {owner}/{kind}/{name}: {exc}")
        _mark_fetched(conn, owner, kind, name)


async def _fetch_versions(
    client: Any,
    conn: sqlite3.Connection,
    sem: asyncio.Semaphore,
    progress: dict[str, int],
    package: tuple[str, str, str],
) -> None:
    owner, kind, name = package
    async with sem:
        label = f"{owner}/{kind}/{name}"
        outcome, body = await _get_versions(client, f"{API}/packages/{label}", label)
        if outcome == "gone":
            _mark_fetched(conn, owner, kind, name)
            return
        if outcome == "failed":
            return
        _store_files(
            conn,
            file_rows(body, owner, kind, name, int(time.time())),
            owner,
            kind,
            name,
        )
        progress["done"] += 1
        if progress["done"] % 200 == 0:
            log(f"catalog metadata {progress['done']}/{progress['total']}")


async def catalog(log_every: int = 500, max_packages: int = 0) -> int:
    import httpx

    conn = db()
    now = int(time.time())
    async with httpx.AsyncClient(
        headers={"User-Agent": USER_AGENT}, timeout=90.0
    ) as client:
        await _search_catalog(client, conn, now, log_every)
        pending = conn.execute(
            "select owner, type, name from packages where versions_fetched = 0"
        ).fetchall()
        if max_packages:
            pending = pending[:max_packages]
        log(f"catalog: {len(pending)} packages still need version metadata")
        sem = asyncio.Semaphore(4)
        progress = {"done": 0, "total": len(pending)}
        tasks = [
            _fetch_versions(client, conn, sem, progress, tuple(p)) for p in pending
        ]
        for chunk_start in range(0, len(tasks), 400):
            await asyncio.gather(*tasks[chunk_start : chunk_start + 400])
    total_files, total_bytes = conn.execute(
        "select count(*), coalesce(sum(size),0) from files"
    ).fetchone()
    log(f"catalog complete: {total_files} files, {total_bytes / 1e9:.2f} GB")
    return 0


# --------------------------------------------------------------------------
# transfer
# --------------------------------------------------------------------------


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


QUOTA_FILE = STATE_DIR / "quota_until"
QUOTA_SLEEP_SECONDS = int(
    os.environ.get("FBUILD_MIRROR_QUOTA_SLEEP_SECONDS", str(24 * 3600))
)


class _QuotaExhausted(Exception):
    """The registry's per-IP daily download cap is spent; wait for the reset."""


class _Throttled(Exception):
    """Registry answered 429; the shared cooldown covers the wait."""


_COOLDOWN_DEADLINE: float = 0.0


def _set_cooldown(seconds: float) -> None:
    global _COOLDOWN_DEADLINE
    _COOLDOWN_DEADLINE = max(_COOLDOWN_DEADLINE, time.monotonic() + seconds)


async def _wait_for_cooldown() -> None:
    while True:
        remaining = _COOLDOWN_DEADLINE - time.monotonic()
        if remaining <= 0:
            return
        await asyncio.sleep(min(remaining, 5.0))


def retry_after_delay(retry_after: str | None, attempt: int) -> float:
    """Seconds to cool down after a 429.

    Retry-After can be huge (the registry has been seen to ask for an hour);
    cap it so the job keeps making progress."""
    try:
        delay = float(retry_after) if retry_after else 0.0
    except ValueError:
        delay = 0.0
    return min(180.0, delay or 10.0 * (attempt + 1))


async def _raise_for_429(r: Any, filename: str, attempt: int) -> None:
    body = (await r.aread())[:400].decode("utf-8", "replace")
    if "Download limit exceeded" in body:
        raise _QuotaExhausted(body)
    delay = retry_after_delay(r.headers.get("retry-after"), attempt)
    _set_cooldown(delay)
    log(f"registry 429 on {filename}: cooling down {delay:.0f}s")
    raise _Throttled(f"429 (cooldown {delay:.0f}s)")


async def _stream_once(
    client: Any, url: str, dest: Path, filename: str, attempt: int
) -> str | None:
    """Stream `url` into `dest`; return an error text when the file is gone upstream."""
    async with client.stream("GET", url) as r:
        if r.status_code in (404, 410):
            return f"HTTP {r.status_code}"
        if r.status_code == 429:
            await _raise_for_429(r, filename, attempt)
        r.raise_for_status()
        with dest.open("wb") as fh:
            async for chunk in r.aiter_bytes(1 << 20):
                fh.write(chunk)
    return None


async def _download(
    client: Any, url: str, dest: Path, filename: str
) -> tuple[str, str | None, int] | None:
    """Download with retries. `None` once `dest` holds the file, else a final result."""
    for attempt in range(6):
        # The registry rate-limits the whole account, not one connection, so a
        # 429 parks every worker until the shared cooldown expires.
        await _wait_for_cooldown()
        try:
            gone = await _stream_once(client, url, dest, filename, attempt)
            return ("gone", gone, 0) if gone else None
        except _Throttled:
            dest.unlink(missing_ok=True)
            if attempt == 5:
                return "error", "HTTP 429 Too Many Requests (gave up)", 0
        except _QuotaExhausted:
            dest.unlink(missing_ok=True)
            raise
        except Exception:
            dest.unlink(missing_ok=True)
            if attempt == 5:
                raise
            await asyncio.sleep(2 * (attempt + 1))
    return None


async def _verify_and_upload(
    s3: Any, dest: Path, sha: str | None, key: str, size: int | None
) -> tuple[str, str | None, int]:
    got = dest.stat().st_size
    if sha:
        actual = await asyncio.to_thread(sha256_file, dest)
        if actual != sha:
            dest.unlink(missing_ok=True)
            return "error", f"sha256 mismatch (registry said {sha}, got {actual})", 0
    extra = {}
    if size and got != size:
        extra["x-registry-size"] = str(size)
    await asyncio.to_thread(
        s3.upload_file,
        str(dest),
        BUCKET,
        key,
        ExtraArgs={"ACL": "public-read", **({} if not extra else {"Metadata": extra})},
    )
    return "done", None, got


async def fetch_one(
    client: Any,
    s3: Any,
    row: tuple[Any, ...],
    workdir: Path,
) -> tuple[str, str | None, int]:
    """Download+verify+upload one file. Returns (state, error, bytes)."""
    url, filename, sha, key, size = row
    dest = workdir / f"{hashlib.sha1(url.encode()).hexdigest()}-{filename}"
    try:
        finished = await _download(client, url, dest, filename)
        if finished is not None:
            return finished
        return await _verify_and_upload(s3, dest, sha, key, size)
    except _QuotaExhausted:
        # Must escape fetch_one: the transfer pass turns it into a 24 h pause.
        raise
    except Exception as exc:  # recorded, retried next pass
        return "error", str(exc)[:500], 0
    finally:
        dest.unlink(missing_ok=True)


async def transfer(limit: int = 0, owner_filter: str | None = None) -> int:
    import httpx

    conn = db()
    s3 = s3_client()
    sql = (
        "select url, filename, sha256, key, size, rowid from files"
        " where state in ('pending','error')"
    )
    params: list[Any] = []
    if owner_filter:
        sql += " and owner = ?"
        params.append(owner_filter)
    sql += " order by (owner='platformio') desc, size desc"
    if limit:
        sql += f" limit {int(limit)}"
    rows = conn.execute(sql, params).fetchall()
    log(f"transfer: {len(rows)} file(s) queued")

    workdir = Path(tempfile.mkdtemp(prefix="fbuild-mirror-"))
    sem = asyncio.Semaphore(CONCURRENCY)
    counters = {"done": 0, "error": 0, "gone": 0, "bytes": 0}
    t0 = time.time()

    async with httpx.AsyncClient(
        headers={"User-Agent": USER_AGENT},
        timeout=httpx.Timeout(60.0, read=300.0),
        follow_redirects=True,
    ) as client:

        async def worker(row: tuple[Any, ...]) -> None:
            async with sem:
                state, err, nbytes = await fetch_one(client, s3, row[:5], workdir)
            with conn:
                conn.execute(
                    "update files set state=?, last_error=?, updated_at=?,"
                    " attempts=attempts+1 where rowid=?",
                    (state, err, int(time.time()), row[5]),
                )
            counters[state] = counters.get(state, 0) + 1
            counters["bytes"] += nbytes
            total_done = counters["done"] + counters["error"] + counters["gone"]
            if total_done % 25 == 0:
                rate = counters["bytes"] / max(1e-9, time.time() - t0) / 1e6
                log(
                    f"transfer {total_done}/{len(rows)} done={counters['done']}"
                    f" err={counters['error']} gone={counters['gone']}"
                    f" {counters['bytes'] / 1e9:.2f}GB @ {rate:.1f}MB/s"
                )

        for start in range(0, len(rows), 200):
            try:
                await asyncio.gather(*(worker(r) for r in rows[start : start + 200]))
            except _QuotaExhausted as exc:
                until = int(time.time()) + QUOTA_SLEEP_SECONDS
                QUOTA_FILE.write_text(str(until))
                log(
                    f"registry quota exhausted ({str(exc)[:120]}); pausing until "
                    f"{time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime(until))}"
                )
                break

    shutil.rmtree(workdir, ignore_errors=True)
    log(f"transfer pass finished: {counters}")
    return 0


# --------------------------------------------------------------------------
# status / verify
# --------------------------------------------------------------------------


def status() -> int:
    conn = db()
    rows = conn.execute(
        "select state, count(*), coalesce(sum(size),0) from files group by state"
    ).fetchall()
    print(f"bucket={BUCKET} region={REGION} db={DB_PATH}")
    for state, count, size in rows:
        print(f"  {state:8} {count:8d} files  {size / 1e9:9.2f} GB")
    tot = conn.execute("select count(*), coalesce(sum(size),0) from files").fetchone()
    print(f"  {'TOTAL':8} {tot[0]:8d} files  {tot[1] / 1e9:9.2f} GB")
    depth = conn.execute(
        "select count(*) from packages p where p.versions_fetched=1"
    ).fetchone()[0]
    allp = conn.execute("select count(*) from packages").fetchone()[0]
    print(f"packages with metadata: {depth}/{allp}")
    return 0


def verify(sample: int = 50) -> int:
    from botocore.exceptions import ClientError

    conn = db()
    s3 = s3_client()
    rows = conn.execute(
        "select key, size from files where state='done' order by random() limit ?",
        (sample,),
    ).fetchall()
    bad = 0
    for key, size in rows:
        try:
            head = s3.head_object(Bucket=BUCKET, Key=key)
        except ClientError as exc:
            print(f"MISSING {key}: {exc}")
            bad += 1
            continue
        if size and head["ContentLength"] != size:
            print(f"SIZE {key}: s3={head['ContentLength']} registry={size}")
            bad += 1
    print(f"verify: {len(rows)} sampled, {bad} problem(s)")
    return 1 if bad else 0


def run() -> int:
    last_catalog = 0.0
    while True:
        try:
            if QUOTA_FILE.exists():
                try:
                    until = int(QUOTA_FILE.read_text().strip() or "0")
                except ValueError:
                    until = 0
                if until > time.time():
                    nap = min(3600, until - int(time.time()))
                    log(f"quota pause active; sleeping {nap}s")
                    time.sleep(max(30, nap))
                    continue
                QUOTA_FILE.unlink(missing_ok=True)
            now = time.monotonic()
            need_catalog = (now - last_catalog) > RECATALOG_SECONDS
            if not need_catalog:
                # Only re-page the registry when the last pass left packages without
                # version metadata; otherwise a 75 s search pass every minute is waste.
                conn = db()
                need_catalog = (
                    conn.execute(
                        "select count(*) from packages where versions_fetched=0"
                    ).fetchone()[0]
                    > 0
                )
                conn.close()
            if need_catalog:
                asyncio.run(catalog())
                last_catalog = time.monotonic()
            asyncio.run(transfer())
            remaining = (
                db()
                .execute(
                    "select count(*) from files where state in ('pending','error')"
                )
                .fetchone()[0]
            )
            log(
                f"run: {remaining} file(s) still unfinished; sleeping before the next pass"
            )
            if remaining == 0:
                time.sleep(RECATALOG_SECONDS)
            else:
                time.sleep(60)
        except KeyboardInterrupt:
            return 0
        except Exception as exc:  # keep the service alive
            log(f"run loop error: {exc}")
            time.sleep(120)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    sub = ap.add_subparsers(dest="cmd", required=True)
    c_cat = sub.add_parser("catalog")
    c_cat.add_argument("--max-packages", type=int, default=0)
    p_t = sub.add_parser("transfer")
    p_t.add_argument("--limit", type=int, default=0)
    p_t.add_argument("--owner", default=None)
    sub.add_parser("run")
    sub.add_parser("status")
    p_v = sub.add_parser("verify")
    p_v.add_argument("--sample", type=int, default=50)
    args = ap.parse_args()
    if args.cmd == "catalog":
        return asyncio.run(catalog(max_packages=args.max_packages))
    if args.cmd == "transfer":
        return asyncio.run(transfer(args.limit, args.owner))
    if args.cmd == "run":
        return run()
    if args.cmd == "status":
        return status()
    if args.cmd == "verify":
        return verify(args.sample)
    return 2


if __name__ == "__main__":
    sys.exit(main())
