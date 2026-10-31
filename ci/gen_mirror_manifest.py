"""Generate fbuild's server-side download mirror manifest (FastLED/fbuild#1810).

Scans the crates for the static PlatformIO-registry and GitHub download URLs
fbuild fetches, checks which ones fbuild's mirror bucket holds, and writes a
zackees/manifest.json v1 ``Release`` listing, for each mirrored artifact,
``urls: [bucket, vendor]`` plus its size and SHA-256 (hashed from the bucket
copy). Clients try the URLs in that order and reject mirror bytes whose hash
does not match.

    uv run --no-project python ci/gen_mirror_manifest.py
    uv run --no-project python ci/gen_mirror_manifest.py --check   # CI drift gate

The bucket key for a vendor URL must match ``mirror_path`` in
``crates/fbuild-packages-fetch/src/downloader_mirrors.rs``.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "site" / "mirror" / "manifest.json"
BUCKET = "https://fbuild.sfo3.digitaloceanspaces.com/"
SCHEMA = "https://zackees.github.io/manifest.json/v1/manifest.schema.json"
REGISTRY_PREFIX = "https://dl.registry.platformio.org/download/"
GITHUB_PREFIX = "https://github.com/"
URL_RE = re.compile(
    r"https://(?:dl\.registry\.platformio\.org/download/|github\.com/[^\s\"'`<>()]+/(?:archive|releases/download)/)"
    r"[^\s\"'`<>(){}\\]+"
)
ARCHIVE_SUFFIXES = (".tar.gz", ".tgz", ".zip", ".tar.xz", ".tar.bz2", ".tar.zst", ".tar")
USER_AGENT = "fbuild-mirror-manifest/1"


def mirror_key(url: str) -> str | None:
    """Bucket key for ``url``; mirrors ``mirror_path`` in downloader_mirrors.rs."""
    if url.startswith(REGISTRY_PREFIX):
        return url[len(REGISTRY_PREFIX) :]
    if url.startswith(GITHUB_PREFIX):
        return "github.com/" + url[len(GITHUB_PREFIX) :]
    return None


def is_test_path(path: str) -> bool:
    parts = path.split("/")
    return "tests" in parts or "test" in Path(path).stem or "fixtures" in parts


def scan_urls() -> list[str]:
    """Static download URLs in tracked, non-test crate sources and data."""
    files = subprocess.run(
        ["git", "ls-files", "crates/**/*.rs", "crates/**/*.json"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.split()
    urls: set[str] = set()
    for rel in files:
        if is_test_path(rel):
            continue
        text = (ROOT / rel).read_text(encoding="utf-8", errors="replace")
        for url in URL_RE.findall(text):
            url = url.rstrip(".,;:")
            if url.endswith(ARCHIVE_SUFFIXES) and "{" not in url:
                urls.add(url)
    return sorted(urls)


def _request(url: str, method: str) -> urllib.request.Request:
    return urllib.request.Request(url, method=method, headers={"User-Agent": USER_AGENT})


def describe(vendor: str, hash_bytes: bool) -> dict | None:
    """A manifest component for ``vendor`` if the bucket holds it, else None."""
    key = mirror_key(vendor)
    if key is None:
        return None
    bucket_url = BUCKET + key
    try:
        with urllib.request.urlopen(_request(bucket_url, "HEAD"), timeout=30) as resp:
            size = int(resp.headers.get("Content-Length", "0"))
    except urllib.error.HTTPError as exc:
        if exc.code in (403, 404):
            return None
        raise
    asset: dict = {
        "filename": vendor.rsplit("/", 1)[-1],
        "size_bytes": size,
        "urls": [bucket_url, vendor],
    }
    if hash_bytes:
        digest = hashlib.sha256()
        seen = 0
        with urllib.request.urlopen(_request(bucket_url, "GET"), timeout=300) as resp:
            while chunk := resp.read(1 << 20):
                digest.update(chunk)
                seen += len(chunk)
        if seen != size:
            raise RuntimeError(f"{bucket_url}: read {seen} bytes, HEAD said {size}")
        asset["sha256"] = digest.hexdigest()
    return {"id": key, "required": False, "asset": asset}


def build(hash_bytes: bool, version: str) -> dict:
    vendors = scan_urls()
    with ThreadPoolExecutor(max_workers=8) as pool:
        found = list(pool.map(lambda v: describe(v, hash_bytes), vendors))
    components = sorted((c for c in found if c), key=lambda c: c["id"])
    print(f"{len(components)} of {len(vendors)} scanned URLs are mirrored", file=sys.stderr)
    return {
        "$schema": SCHEMA,
        "kind": "Release",
        "schema_version": 1,
        "tool": "fbuild-download-mirror",
        "version": version,
        "components": components,
    }


def render(doc: dict) -> str:
    return json.dumps(doc, indent=2, sort_keys=False) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--out", type=Path, default=OUT)
    parser.add_argument("--version", help="manifest version (default: keep the existing one)")
    parser.add_argument("--no-sha256", action="store_true", help="skip hashing bucket objects")
    parser.add_argument("--check", action="store_true", help="fail if --out is out of date")
    args = parser.parse_args()

    existing = json.loads(args.out.read_text()) if args.out.exists() else {}
    version = args.version or existing.get("version") or "1"
    rendered = render(build(not args.no_sha256, version))
    if args.check:
        if args.out.exists() and args.out.read_text() == rendered:
            return 0
        print(f"{args.out} is out of date; rerun ci/gen_mirror_manifest.py", file=sys.stderr)
        return 1
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(rendered)
    print(f"wrote {args.out}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
