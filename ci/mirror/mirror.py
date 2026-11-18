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
catalog   Enumerate the registry into the local SQLite manifest. The first
          pass reads every package; later passes page the registry search
          newest-updated first, stop at the DB's watermark and re-read the
          versions of every package updated since, so a new release of a
          known package is queued too. Resumable and idempotent.
transfer  Download every pending file from the registry, verify sha256, upload
          it to Spaces with a public-read ACL, mark it done. Resumable; safe to
          re-run after a crash or reboot.
run       catalog then transfer, forever (re-catalogs periodically to pick up
          new registry versions). Exits 3 once the registry refuses the client.
status    Print manifest progress by state.
verify    Re-HEAD a sample (or all) of the done rows against Spaces.

The registry's CDN rejects requests whose User-Agent does not start with
`PlatformIO/` (HTTP 403 "This download method is not supported"), so every
registry request sends a `PlatformIO/` User-Agent: `FBUILD_MIRROR_UA` when set,
otherwise fbuild's own production format (see `resolve_user_agent`). Any 403
from the registry stops the run (exit 3) instead of failing file after file:
it refuses the client, not one file (FastLED/fbuild#2040).
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
import tomllib
from collections.abc import Callable, Iterable, Mapping
from datetime import datetime
from pathlib import Path
from typing import Any, cast

# boto3/httpx are imported where they are used, so the pure helpers can be
# imported (and unit tested) without the script's dependencies installed.

API = "https://api.registry.platformio.org/v3"
DL_PREFIX = "https://dl.registry.platformio.org/download/"
# The User-Agent. The default is fbuild's production format,
# `registry_user_agent()` in crates/fbuild-core/src/http.rs (ci/test_mirror_tool.py
# keeps the two in step). The old `PlatformIO/6.1.16 (fbuild-mirror; +https://...)`
# now gets 403 "This download method is not supported": on 2026-11-17 every UA
# carrying the `fbuild-mirror` token was refused and the same strings without it
# were served, whatever the PlatformIO version or the `+https://` comment
# (FastLED/fbuild#2040 item 3). `FBUILD_MIRROR_UA` overrides it; the CDN's daily
# download quota has also looked keyed on the exact string.
UA_ENV = "FBUILD_MIRROR_UA"
PIO_CORE_VERSION = "6.2.0"
# Present when the script runs from a checkout; on a droplet rotate.py passes
# FBUILD_MIRROR_UA instead.
REPO_CARGO_TOML = Path(__file__).resolve().parent.parent.parent / "Cargo.toml"


def fbuild_version(cargo_toml: Path) -> str | None:
    """`[workspace.package] version` of an fbuild checkout, if there is one."""
    try:
        data = tomllib.loads(cargo_toml.read_text())
    except (OSError, tomllib.TOMLDecodeError):
        return None
    version = data.get("workspace", {}).get("package", {}).get("version")
    return version if isinstance(version, str) and version else None


def default_user_agent(version: str) -> str:
    """fbuild's production User-Agent for fbuild `version`."""
    return f"PlatformIO/{PIO_CORE_VERSION} (fbuild/{version})"


def resolve_user_agent(
    env: Mapping[str, str], cargo_toml: Path = REPO_CARGO_TOML
) -> str:
    """`FBUILD_MIRROR_UA`, else the production UA for the checkout's version.

    Raises ValueError rather than sending a UA the CDN is known to refuse. The
    value also lands in a systemd `Environment="..."` line on the droplet, so
    quotes, backslashes, `%` and control characters are refused too."""
    user_agent = env.get(UA_ENV, "").strip()
    if not user_agent:
        version = fbuild_version(cargo_toml)
        if version is None:
            raise ValueError(
                f"{UA_ENV} is unset and there is no fbuild Cargo.toml at {cargo_toml};"
                " set it (`rotate.py --print-user-agent` prints the default)"
            )
        user_agent = default_user_agent(version)
    if not user_agent.startswith("PlatformIO/"):
        raise ValueError(
            f"User-Agent {user_agent!r}: the registry CDN refuses any User-Agent"
            " that does not start with 'PlatformIO/'"
        )
    if any(ord(ch) < 0x20 or ch in '"%\\' for ch in user_agent):
        raise ValueError(
            f"User-Agent {user_agent!r}: quotes, backslashes, % and control"
            " characters are not allowed"
        )
    return user_agent


STATE_DIR = Path(os.environ.get("FBUILD_MIRROR_STATE", "/var/lib/fbuild-mirror"))
DB_PATH = STATE_DIR / "manifest.db"
CREDS_PATH = Path(
    os.environ.get("FBUILD_MIRROR_CREDS", "/etc/fbuild-mirror/spaces.json")
)
# Written when the registry refuses the client (any 403); `run` will not start
# again while it exists, and rotate.py stops the supervisor when it sees it.
REFUSED_FILE = STATE_DIR / "registry_refused"
EXIT_REFUSED = 3
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
create table if not exists meta (
  key text primary key,
  value text not null
);
"""

# `meta` key: the registry `updated_at` (epoch seconds) up to which every
# package's versions are catalogued. A pass rescans from a little before it.
WATERMARK_KEY = "registry_updated_since"
WATERMARK_OVERLAP_SECONDS = 6 * 3600


def configure(state_dir: Path, creds: Path) -> None:
    """Point the module at a local state dir and credentials file.

    On a droplet the unit file sets `FBUILD_MIRROR_STATE`/`_CREDS`; a local
    pass (ci/mirror/update.py) calls this instead."""
    global STATE_DIR, DB_PATH, CREDS_PATH, QUOTA_FILE, REFUSED_FILE
    STATE_DIR = state_dir
    DB_PATH = state_dir / "manifest.db"
    CREDS_PATH = creds
    QUOTA_FILE = state_dir / "quota_until"
    REFUSED_FILE = state_dir / "registry_refused"


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


class RegistryRefused(Exception):
    """The registry answered 403. It refuses the client (in practice the
    User-Agent), so every other request would fail the same way: stop."""


def check_refused(status: int, url: str, body: str) -> None:
    if status == 403:
        raise RegistryRefused(f"HTTP 403 from {url}: {body.strip()[:300]}")


def record_refusal(exc: RegistryRefused) -> None:
    """Make the refusal loud and sticky (see `REFUSED_FILE`)."""
    log(f"REGISTRY REFUSED THIS CLIENT; stopping. {exc}")
    REFUSED_FILE.parent.mkdir(parents=True, exist_ok=True)
    REFUSED_FILE.write_text(f"{exc}\n")


# --------------------------------------------------------------------------
# watermark
# --------------------------------------------------------------------------


def registry_time(value: Any) -> int | None:
    """Epoch seconds of a registry timestamp such as `2026-11-17T21:24:52Z`."""
    if not isinstance(value, str) or not value:
        return None
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    return int(parsed.timestamp()) if parsed.tzinfo else None


def stored_watermark(conn: sqlite3.Connection) -> int | None:
    row = conn.execute(
        "select value from meta where key = ?", (WATERMARK_KEY,)
    ).fetchone()
    return int(row[0]) if row else None


def store_watermark(conn: sqlite3.Connection, value: int) -> None:
    with conn:
        conn.execute(
            "insert or replace into meta (key, value) values (?, ?)",
            (WATERMARK_KEY, str(value)),
        )


def watermark_may_advance(
    stored: int | None, scan_from: int | None, fresh_db: bool
) -> bool:
    """May a finished scan record a new watermark?

    Only when it covered everything since the old one: a full scan of an empty
    DB, or an incremental scan that started at or before the stored watermark.
    With none stored, an explicit `--since` is the operator's word that the DB
    covers everything before it. A full scan of a DB without a watermark (one
    catalogued before it existed) proves nothing about known packages, and
    neither does a later `--since`."""
    if scan_from is None:
        return fresh_db
    return stored is None or scan_from <= stored


def split_updated(
    items: list[dict[str, Any]], cutoff: int | None
) -> tuple[list[dict[str, Any]], bool]:
    """`(items updated at or after cutoff, whether the cutoff was reached)`.

    Search results come newest-updated first, so the first older item ends
    the scan. An item without a readable `updated_at` is kept."""
    if cutoff is None:
        return items, False
    fresh = []
    for item in items:
        updated = registry_time(item.get("updated_at"))
        if updated is not None and updated < cutoff:
            return fresh, True
        fresh.append(item)
    return fresh, False


def newest_update(items: Iterable[dict[str, Any]], newest: int | None) -> int | None:
    for item in items:
        updated = registry_time(item.get("updated_at"))
        if updated is not None and (newest is None or updated > newest):
            newest = updated
    return newest


# --------------------------------------------------------------------------
# catalog
# --------------------------------------------------------------------------


async def _search_page(client: Any, page: int) -> Any:
    """One `/search` page, newest-updated first, retried; the registry is flaky."""
    params = {"query": "*", "page": page, "limit": 50, "sort": "updated"}
    for attempt in range(6):
        try:
            r = await client.get(f"{API}/search", params=params)
            check_refused(r.status_code, f"{API}/search", r.text)
            r.raise_for_status()
            return r
        except RegistryRefused:
            raise
        except Exception as exc:  # registry is flaky
            log(f"search page {page} attempt {attempt + 1} failed: {exc}")
            await asyncio.sleep(2 * (attempt + 1))
    raise SystemExit(f"search page {page} kept failing; stopping (resumable)")


def _record_packages(
    conn: sqlite3.Connection,
    items: list[dict[str, Any]],
    now: int,
    refresh: bool = False,
) -> None:
    """Upsert search results. `refresh` queues their versions to be read again
    (an incremental scan); otherwise only unknown packages are queued."""
    with conn:
        for it in items:
            owner = (it.get("owner") or {}).get("username") or "?"
            conn.execute(
                "insert or replace into packages"
                " (owner, type, name, tier, seen_at, versions_fetched)"
                " values (?,?,?,?,?, case when ? then 0 else coalesce((select"
                "  versions_fetched from packages where owner=? and type=? and name=?),"
                "  0) end)",
                (
                    owner,
                    it.get("type"),
                    it.get("name"),
                    it.get("tier"),
                    now,
                    int(refresh),
                    owner,
                    it.get("type"),
                    it.get("name"),
                ),
            )


def request_refresh(
    conn: sqlite3.Connection, packages: Iterable[tuple[str, str, str]], now: int
) -> None:
    """Queue named `(owner, type, name)` packages for a version refresh."""
    with conn:
        for owner, kind, name in packages:
            conn.execute(
                "insert into packages (owner, type, name, seen_at, versions_fetched)"
                " values (?,?,?,?,0) on conflict (owner, type, name)"
                " do update set versions_fetched = 0",
                (owner, kind, name, now),
            )


async def _search_catalog(
    client: Any,
    conn: sqlite3.Connection,
    now: int,
    log_every: int,
    cutoff: int | None = None,
) -> int | None:
    """Page the registry search, newest-updated first, into `packages`.

    With `cutoff`, stop at the first package last updated before it and queue
    every package seen for a version refresh. Returns the newest `updated_at`
    seen, or `None` when the scan saw nothing."""
    page = 1
    seen = 0
    newest = None
    while True:
        data = (await _search_page(client, page)).json()
        items, reached = split_updated(data["items"], cutoff)
        _record_packages(conn, items, now, refresh=cutoff is not None)
        newest = newest_update(items, newest)
        seen += len(data["items"])
        if seen % log_every < len(data["items"]):
            log(f"catalog search {seen}/{data['total']}")
        if reached or not data["items"] or seen >= data["total"]:
            return newest
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
            check_refused(rr.status_code, url, rr.text)
            rr.raise_for_status()
            return "ok", rr.json()
        except RegistryRefused:
            raise
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


async def _scan(
    client: Any, conn: sqlite3.Connection, now: int, log_every: int, since: int | None
) -> None:
    """Search the registry from `since` (else the stored watermark; else all of
    it) and record the newest `updated_at` seen when the scan earned it."""
    stored = stored_watermark(conn)
    fresh_db = conn.execute("select count(*) from packages").fetchone()[0] == 0
    scan_from = since if since is not None else stored
    if scan_from is None and not fresh_db:
        log(
            "WARNING no watermark in this DB: only packages it has never seen are"
            " catalogued; run `update.py --since <date>` once to pick up new"
            " releases of known packages"
        )
    cutoff = None if scan_from is None else scan_from - WATERMARK_OVERLAP_SECONDS
    newest = await _search_catalog(client, conn, now, log_every, cutoff)
    if newest is not None and watermark_may_advance(stored, scan_from, fresh_db):
        store_watermark(conn, max(newest, stored or 0))


async def _fetch_pending_versions(
    client: Any, conn: sqlite3.Connection, max_packages: int
) -> None:
    pending = conn.execute(
        "select owner, type, name from packages where versions_fetched = 0"
    ).fetchall()
    if max_packages:
        pending = pending[:max_packages]
    log(f"catalog: {len(pending)} packages still need version metadata")
    sem = asyncio.Semaphore(4)
    progress = {"done": 0, "total": len(pending)}
    tasks = [_fetch_versions(client, conn, sem, progress, tuple(p)) for p in pending]
    for chunk_start in range(0, len(tasks), 400):
        await asyncio.gather(*tasks[chunk_start : chunk_start + 400])


async def catalog(
    user_agent: str,
    log_every: int = 500,
    max_packages: int = 0,
    since: int | None = None,
    packages: Iterable[tuple[str, str, str]] = (),
) -> int:
    """Queue every registry file the DB lacks.

    `packages` refreshes only those packages and leaves the watermark alone;
    otherwise the registry search runs from `since` or the stored watermark."""
    import httpx

    conn = db()
    now = int(time.time())
    packages = list(packages)
    async with httpx.AsyncClient(
        headers={"User-Agent": user_agent}, timeout=90.0
    ) as client:
        if packages:
            request_refresh(conn, packages, now)
        else:
            await _scan(client, conn, now, log_every, since)
        await _fetch_pending_versions(client, conn, max_packages)
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
        if r.status_code == 403:
            body = (await r.aread())[:400].decode("utf-8", "replace")
            check_refused(r.status_code, url, body)
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
        except (_QuotaExhausted, RegistryRefused):
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
    except (_QuotaExhausted, RegistryRefused):
        # Must escape fetch_one: the transfer pass turns the quota into a 24 h
        # pause and a refusal into a stop.
        raise
    except Exception as exc:  # recorded, retried next pass
        return "error", str(exc)[:500], 0
    finally:
        dest.unlink(missing_ok=True)


async def transfer(
    user_agent: str, limit: int = 0, owner_filter: str | None = None
) -> int:
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
        headers={"User-Agent": user_agent},
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

        status = None
        for start in range(0, len(rows), 200):
            status = await run_chunk(worker, rows[start : start + 200])
            if status is not None:
                break

    shutil.rmtree(workdir, ignore_errors=True)
    log(f"transfer pass finished: {counters}")
    return status or 0


def _pause_for_quota(exc: _QuotaExhausted) -> None:
    until = int(time.time()) + QUOTA_SLEEP_SECONDS
    QUOTA_FILE.write_text(str(until))
    log(
        f"registry quota exhausted ({str(exc)[:120]}); pausing until "
        f"{time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime(until))}"
    )


async def run_chunk(
    worker: Callable[[tuple[Any, ...]], Any], rows: list[tuple[Any, ...]]
) -> int | None:
    """Run `worker` over `rows`; `None` to go on, else the pass's exit status.

    A spent quota or a refusal cancels the chunk's other downloads, so their
    rows stay queued instead of being recorded as errors."""
    try:
        async with asyncio.TaskGroup() as group:
            for row in rows:
                group.create_task(worker(row))
    except ExceptionGroup as errors:  # builtin on 3.11+, which the script requires
        refused = errors.subgroup(RegistryRefused)
        quota = errors.subgroup(_QuotaExhausted)
        if refused is not None:
            # TaskGroup groups the tasks' own (leaf) exceptions one level deep.
            record_refusal(cast(RegistryRefused, refused.exceptions[0]))
            return EXIT_REFUSED
        if quota is not None:
            _pause_for_quota(cast(_QuotaExhausted, quota.exceptions[0]))
            return 0
        raise
    return None


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


def _quota_pause() -> int | None:
    """Seconds still to wait for the daily quota, or `None` when it has reset."""
    if not QUOTA_FILE.exists():
        return None
    try:
        until = int(QUOTA_FILE.read_text().strip() or "0")
    except ValueError:
        until = 0
    if until > time.time():
        return min(3600, until - int(time.time()))
    QUOTA_FILE.unlink(missing_ok=True)
    return None


def _needs_catalog(last_catalog: float) -> bool:
    if (time.monotonic() - last_catalog) > RECATALOG_SECONDS:
        return True
    # Only re-page the registry when the last pass left packages without
    # version metadata; otherwise a 75 s search pass every minute is waste.
    conn = db()
    try:
        return (
            conn.execute(
                "select count(*) from packages where versions_fetched=0"
            ).fetchone()[0]
            > 0
        )
    finally:
        conn.close()


def run(user_agent: str) -> int:
    if REFUSED_FILE.exists():
        log(f"registry refused this client earlier: {REFUSED_FILE.read_text().strip()}")
        return EXIT_REFUSED
    last_catalog = 0.0
    while True:
        try:
            nap = _quota_pause()
            if nap is not None:
                log(f"quota pause active; sleeping {nap}s")
                time.sleep(max(30, nap))
                continue
            if _needs_catalog(last_catalog):
                asyncio.run(catalog(user_agent))
                last_catalog = time.monotonic()
            if asyncio.run(transfer(user_agent)) == EXIT_REFUSED:
                return EXIT_REFUSED
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
        except RegistryRefused as exc:
            record_refusal(exc)
            return EXIT_REFUSED
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
    if args.cmd == "status":
        return status()
    if args.cmd == "verify":
        return verify(args.sample)
    try:
        user_agent = resolve_user_agent(os.environ)
    except ValueError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    log(f"registry User-Agent: {user_agent}")
    if args.cmd == "run":
        return run(user_agent)
    try:
        if args.cmd == "catalog":
            return asyncio.run(catalog(user_agent, max_packages=args.max_packages))
        return asyncio.run(transfer(user_agent, args.limit, args.owner))
    except RegistryRefused as exc:
        record_refusal(exc)
        return EXIT_REFUSED


if __name__ == "__main__":
    sys.exit(main())
