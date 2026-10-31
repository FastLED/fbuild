"""Generate fbuild's server-side download mirror manifest (FastLED/fbuild#1810, #1815).

Lists every object in fbuild's mirror bucket, SHA-256-hashes the ones not yet
recorded (bytes are read from the bucket, never from PlatformIO), and writes
zackees/manifest.json v1 documents under ``site/mirror/``:

- ``manifest.json``: an ``Index`` whose ``tools`` map each bucket owner (the
  first key segment, a registry owner) to a shard
  descriptor ``{url, sha256, size_bytes}``;
- ``shards/<id>.json``: one ``Release`` per owner whose components list, for
  each object, ``urls: [bucket, vendor]`` plus ``size_bytes`` and ``sha256``,
  and in ``provides`` the PlatformIO ``system`` values a host-specific
  archive serves (absent means any host);
- ``shards/names-<h>.json``: listed under the Index keys ``@names-<h>``, one
  component per package with id ``<type>/<name>/<owner>`` and one of its
  archives as the asset, so fbuild can find the owner of an owner-less spec
  such as ``lib_deps = FastLED``. ``<h>`` is the first hex digit of the
  SHA-256 of the ASCII-lowercased name.

Together these are fbuild's registry index (FastLED/fbuild#1829): fbuild
resolves package versions from them instead of PlatformIO's registry API.
The ``system`` values come from the mirror's own state database
(``_state/manifest.db`` in the bucket, or ``--state-db``).

Re-runs are incremental: an object already in a shard with the same size is
not downloaded again.

    uv run --no-project --with boto3 python ci/gen_mirror_manifest.py
    uv run --no-project --with boto3 python ci/gen_mirror_manifest.py --check

Listing the bucket needs read credentials: ``--credentials`` (a JSON file
with ``access_key``/``secret_key``) or ``AWS_ACCESS_KEY_ID`` /
``AWS_SECRET_ACCESS_KEY``. Secrets are never printed.

Bucket keys must match ``mirror_path`` in
``crates/fbuild-packages-fetch/src/downloader_mirrors.rs``.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sqlite3
import sys
import tempfile
import threading
import urllib.request
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path
from urllib.parse import quote

ROOT = Path(__file__).resolve().parent.parent
OUT_DIR = ROOT / "site" / "mirror"
BUCKET_NAME = "fbuild"
REGION = "sfo3"
BUCKET = f"https://{BUCKET_NAME}.{REGION}.digitaloceanspaces.com/"
SCHEMA = "https://zackees.github.io/manifest.json/v1/manifest.schema.json"
REGISTRY_PREFIX = "https://dl.registry.platformio.org/download/"
TOOL = "fbuild-download-mirror"
USER_AGENT = "fbuild-mirror-manifest/2"
STATE_DB_KEY = "_state/manifest.db"
NAMES_KEY = "@names-"  # '@' is escaped in owner keys, so no owner collides


def vendor_url(key: str) -> str:
    """Original registry URL for a bucket key; inverse of ``mirror_path``.

    Keys hold the registry path decoded (``dmytro korniienko/...``), so it is
    re-escaped the way the registry's own download URLs are; an existing
    ``%XX`` is kept as is."""
    return REGISTRY_PREFIX + quote(key, safe="/%")


def bucket_url(key: str) -> str:
    # Keys copied from registry paths can hold a literal ``%20``; the URL
    # must escape it so Spaces finds the key as stored.
    return BUCKET + quote(key, safe="/")


def is_registry_key(key: str) -> bool:
    """``<owner>/<type>/<name>/<version>/<file>``: the registry layout the
    bucket mirrors. Anything else (bucket bookkeeping such as ``_state/``)
    is not a download and is left out."""
    parts = key.split("/")
    return len(parts) == 5 and all(parts) and not parts[0].startswith("_")


def owner_of(key: str) -> str:
    """Index key for ``key``: its first segment, escaped like the URL fbuild
    downloads, so the client can take it straight from that URL."""
    return quote(key.split("/", 1)[0], safe="%")


def shard_id(owner: str) -> str:
    return hashlib.sha256(owner.encode()).hexdigest()[:16]


def s3_client(credentials: Path | None):
    import boto3  # only the bucket steps need it

    if credentials:
        creds = json.loads(credentials.read_text())
        key_id, secret = creds["access_key"], creds["secret_key"]
    else:
        key_id, secret = os.environ["AWS_ACCESS_KEY_ID"], os.environ["AWS_SECRET_ACCESS_KEY"]
    return boto3.client(
        "s3",
        region_name=REGION,
        endpoint_url=f"https://{REGION}.digitaloceanspaces.com",
        aws_access_key_id=key_id,
        aws_secret_access_key=secret,
    )


def list_bucket(s3) -> dict[str, int]:
    objects: dict[str, int] = {}
    for page in s3.get_paginator("list_objects_v2").paginate(Bucket=BUCKET_NAME):
        for obj in page.get("Contents", []):
            if is_registry_key(obj["Key"]):
                objects[obj["Key"]] = obj["Size"]
    return objects


def load_systems(s3, state_db: Path | None) -> dict[str, list[str]]:
    """``key -> systems`` from the mirror's state database; ``[]`` means any
    host. Read from ``state_db`` or downloaded from the bucket."""
    with tempfile.TemporaryDirectory() as tmp:
        if state_db is None:
            state_db = Path(tmp) / "manifest.db"
            s3.download_file(BUCKET_NAME, STATE_DB_KEY, str(state_db))
        db = sqlite3.connect(f"file:{state_db}?mode=ro", uri=True)
        try:
            rows = db.execute("select key, system from files where state = 'done'").fetchall()
        finally:
            db.close()
    systems: dict[str, list[str]] = {}
    for key, system in rows:
        if system in (None, "", "*"):
            systems[key] = []
            continue
        value = json.loads(system) if system.startswith("[") else [system]
        if not isinstance(value, list) or not all(isinstance(v, str) and v for v in value):
            raise ValueError(f"{key}: unreadable system {system!r}")
        systems[key] = [] if "*" in value else value
    return systems


def load_existing(out_dir: Path) -> dict[str, tuple[int, str]]:
    """``key -> (size, sha256)`` from the shards already on disk."""
    known: dict[str, tuple[int, str]] = {}
    for shard in (out_dir / "shards").glob("*.json"):
        for component in json.loads(shard.read_text()).get("components", []):
            asset = component.get("asset")
            if asset is None or shard.name.startswith("names-"):
                continue
            known[component["id"]] = (asset["size_bytes"], asset["sha256"])
    return known


def hash_object(key: str, size: int) -> str:
    request = urllib.request.Request(bucket_url(key), headers={"User-Agent": USER_AGENT})
    digest = hashlib.sha256()
    seen = 0
    with urllib.request.urlopen(request, timeout=600) as resp:
        while chunk := resp.read(1 << 20):
            digest.update(chunk)
            seen += len(chunk)
    if seen != size:
        raise RuntimeError(f"{key}: read {seen} bytes, listing said {size}")
    return digest.hexdigest()


def load_checkpoint(path: Path | None) -> dict[str, tuple[int, str]]:
    """Hashes recorded by an interrupted run (``size\tsha256\tkey`` lines)."""
    if not path or not path.exists():
        return {}
    done = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        size, sha, key = line.split("\t", 2)
        done[key] = (int(size), sha)
    return done


def hash_missing(
    objects: dict[str, int],
    known: dict[str, tuple[int, str]],
    workers: int,
    checkpoint: Path | None,
) -> dict[str, tuple[int, str]]:
    known = {**known, **load_checkpoint(checkpoint)}
    log = checkpoint.open("a", encoding="utf-8") if checkpoint else None
    todo = {k: s for k, s in objects.items() if known.get(k, (None,))[0] != s}
    total = sum(todo.values())
    print(f"{len(todo)} of {len(objects)} objects need hashing ({total / 1e9:.1f} GB)", file=sys.stderr)
    result = {k: v for k, v in known.items() if k in objects and k not in todo}
    done_bytes = 0
    lock = threading.Lock()
    failures: list[str] = []
    with ThreadPoolExecutor(max_workers=workers) as pool:
        futures = {pool.submit(hash_object, k, s): (k, s) for k, s in todo.items()}
        for n, future in enumerate(as_completed(futures), 1):
            key, size = futures[future]
            try:
                result[key] = (size, future.result())
                if log:
                    log.write(f"{size}\t{result[key][1]}\t{key}\n")
                    log.flush()
            except Exception as exc:  # report and keep going
                failures.append(f"{key}: {exc}")
            with lock:
                done_bytes += size
            if n % 500 == 0 or n == len(futures):
                print(f"  {n}/{len(futures)} ({done_bytes / 1e9:.1f}/{total / 1e9:.1f} GB)", file=sys.stderr)
    if failures:
        print(f"{len(failures)} objects failed to hash:", file=sys.stderr)
        for line in failures[:50]:
            print("  " + line, file=sys.stderr)
    return result


def render(doc: dict) -> str:
    return json.dumps(doc, indent=1, ensure_ascii=False) + "\n"


def asset_of(key: str, size: int, sha: str, systems: list[str]) -> dict:
    asset = {
        "filename": key.rsplit("/", 1)[-1],
        "size_bytes": size,
        "sha256": sha,
        "urls": [bucket_url(key), vendor_url(key)],
    }
    if systems:
        asset["provides"] = systems
    return asset


def descriptor(rel: str, text: str) -> dict:
    data = text.encode()
    return {
        "descriptor": {"url": rel, "sha256": hashlib.sha256(data).hexdigest(), "size_bytes": len(data)},
        "kind_hint": "data",
    }


def names_bucket(name: str) -> str:
    """Names shard for a package name; mirrors ``names_key`` in the client."""
    return hashlib.sha256(name.encode().lower()).hexdigest()[0]


def names_documents(
    hashed: dict[str, tuple[int, str]], systems: dict[str, list[str]], version: str
) -> dict[str, str]:
    """Bucket digit -> rendered names shard."""
    first_key: dict[str, str] = {}
    for key in sorted(hashed):
        owner, kind, name = key.split("/")[:3]
        first_key.setdefault(f"{kind}/{name}/{owner}", key)
    buckets: dict[str, list[dict]] = {}
    for package, key in sorted(first_key.items()):
        buckets.setdefault(names_bucket(package.split("/")[1]), []).append(
            {"id": package, "required": False, "asset": asset_of(key, *hashed[key], systems[key])}
        )
    return {
        bucket: render(
            {
                "$schema": SCHEMA,
                "kind": "Release",
                "schema_version": 1,
                "tool": f"{TOOL}/names-{bucket}",
                "version": version,
                "components": components,
            }
        )
        for bucket, components in sorted(buckets.items())
    }


def build_documents(
    hashed: dict[str, tuple[int, str]], systems: dict[str, list[str]], version: str
) -> dict[str, str]:
    """Relative path -> rendered document."""
    unknown = sorted(set(hashed) - set(systems))
    if unknown:
        raise ValueError(f"{len(unknown)} objects have no system in the state db, e.g. {unknown[:3]}")
    by_owner: dict[str, list[str]] = {}
    for key in hashed:
        by_owner.setdefault(owner_of(key), []).append(key)
    docs: dict[str, str] = {}
    tools: dict[str, dict] = {}
    for owner in sorted(by_owner):
        components = [
            {"id": key, "required": False, "asset": asset_of(key, *hashed[key], systems[key])}
            for key in sorted(by_owner[owner])
        ]
        shard = render(
            {
                "$schema": SCHEMA,
                "kind": "Release",
                "schema_version": 1,
                "tool": f"{TOOL}/{owner}",
                "version": version,
                "components": components,
            }
        )
        rel = f"shards/{shard_id(owner)}.json"
        docs[rel] = shard
        tools[owner] = descriptor(rel, shard)
    for bucket, text in names_documents(hashed, systems, version).items():
        rel = f"shards/names-{bucket}.json"
        docs[rel] = text
        tools[NAMES_KEY + bucket] = descriptor(rel, text)
    docs["manifest.json"] = render(
        {"$schema": SCHEMA, "kind": "Index", "schema_version": 1, "tools": tools}
    )
    return docs


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--out-dir", type=Path, default=OUT_DIR)
    parser.add_argument("--credentials", type=Path, help="JSON file with access_key/secret_key")
    parser.add_argument("--state-db", type=Path, help=f"local copy of the bucket's {STATE_DB_KEY}")
    parser.add_argument("--version", help="shard version (default: keep the existing one)")
    parser.add_argument("--workers", type=int, default=32)
    parser.add_argument("--checkpoint", type=Path, help="append hashes here as they finish; reused on rerun")
    parser.add_argument("--check", action="store_true", help="fail if the documents are out of date")
    args = parser.parse_args()

    s3 = s3_client(args.credentials)
    objects = list_bucket(s3)
    known = load_existing(args.out_dir)
    if args.check:
        stale = sorted(k for k, s in objects.items() if known.get(k, (None,))[0] != s)
        gone = sorted(set(known) - set(objects))
        if stale or gone:
            print(f"{len(stale)} unhashed/changed and {len(gone)} removed objects; rerun", file=sys.stderr)
            return 1
        return 0

    hashed = hash_missing(objects, known, args.workers, args.checkpoint)
    missing = set(objects) - set(hashed)
    version = args.version or "1"
    docs = build_documents(hashed, load_systems(s3, args.state_db), version)
    shards_dir = args.out_dir / "shards"
    shards_dir.mkdir(parents=True, exist_ok=True)
    keep = {args.out_dir / rel for rel in docs}
    for old in shards_dir.glob("*.json"):
        if old not in keep:
            old.unlink()
    for rel, text in docs.items():
        (args.out_dir / rel).write_text(text, encoding="utf-8")
    print(f"wrote {len(docs) - 1} shards covering {len(hashed)} objects", file=sys.stderr)
    if missing:
        print(f"{len(missing)} objects left unhashed; rerun to retry", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
