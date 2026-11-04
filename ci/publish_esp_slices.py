# /// script
# requires-python = ">=3.11"
# dependencies = ["boto3>=1.34", "zstandard>=0.23"]
# ///
"""Publish one ESP libs version as per-chip slices plus a shard component.

The hot path is `fbuild`'s: it resolves a vendor archive URL from the platform
manifest, looks that exact URL up in the `@esp-libs` shard, and downloads the
chip slice when the entry carries one. This script performs the server side of
that contract for one version, so covering another platform release is a
command rather than a hand edit.

    uv run --script ci/publish_esp_slices.py \
        --source /path/esp32-core-3.3.11-libs.tar.xz \
        --vendor-url https://github.com/espressif/arduino-esp32/releases/download/3.3.11/esp32-core-3.3.11-libs.tar.xz \
        --shard site/mirror/shards/esp-libs.json \
        --index site/mirror/manifest.json \
        --credentials ~/.config/do-spaces/credentials.json

`--source` may be a local archive or an https URL. Objects land under
`v1/espressif/esp32-arduino-libs/release/<version>/<chip>/` with the whole
bundle mirrored under `whole/`, all `public-read` so clients need no auth.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sys
import tempfile
import urllib.request
from pathlib import Path

import boto3

SCHEMA = "https://zackees.github.io/manifest.json/v1/manifest.schema.json"
BUCKET = "fbuild"
ENDPOINT = "https://sfo3.digitaloceanspaces.com"
PUBLIC_BASE = "https://fbuild.sfo3.digitaloceanspaces.com"
KEY_ROOT = "v1/espressif/esp32-arduino-libs/release"
NAME = "framework-arduinoespressif32-libs"
SHARD_TOOL = "fbuild-download-mirror/esp-libs"
INDEX_OWNER = "@esp-libs"


def digest(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def version_of(vendor_url: str) -> str:
    """Mirror `libs.rs`: ``esp32-core-3.3.12-libs.tar.xz`` -> ``core-3.3.12``."""
    name = vendor_url.rsplit("/", 1)[-1]
    return name.removeprefix("esp32-").removesuffix("-libs.tar.xz")


def slice_archive(source: Path, version: str, out_dir: Path) -> dict:
    """Run the slicer as a subprocess so its PEP 723 deps stay independent."""
    script = Path(__file__).with_name("slice_esp_libs.py")
    result = subprocess.run(
        [
            sys.executable,
            str(script),
            str(source),
            "--version",
            version,
            "--out-dir",
            str(out_dir),
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(result.stdout)


def downloaded(source: str, scratch: Path) -> Path:
    if not source.startswith(("https://", "http://")):
        return Path(source)
    target = scratch / Path(source).name
    with urllib.request.urlopen(source) as response, target.open("wb") as handle:
        while chunk := response.read(1 << 20):
            handle.write(chunk)
    return target


def upload(s3, path: Path, key: str) -> None:
    s3.upload_file(str(path), BUCKET, key)
    s3.put_object_acl(Bucket=BUCKET, Key=key, ACL="public-read")


def publish(
    source: str, vendor_url: str, shard_path: Path, index_path: Path, credentials: Path
) -> dict:
    version = version_of(vendor_url)
    if not version:
        raise ValueError(f"cannot derive a version from {vendor_url}")
    creds = json.loads(credentials.read_text())
    s3 = boto3.client(
        "s3",
        region_name="sfo3",
        endpoint_url=ENDPOINT,
        aws_access_key_id=creds["access_key"],
        aws_secret_access_key=creds["secret_key"],
    )
    with tempfile.TemporaryDirectory(prefix="esp-publish-") as tmp:
        scratch = Path(tmp)
        archive = downloaded(source, scratch)
        sliced = slice_archive(archive, version, scratch / "slices")
        whole_sha = digest(archive)
        size_bytes = archive.stat().st_size
        whole_key = (
            f"{KEY_ROOT}/{version}/whole/{NAME}-{version}.{whole_sha[:16]}.tar.xz"
        )
        upload(s3, archive, whole_key)
        chips = {}
        for record in sliced["slices"]:
            key = f"{KEY_ROOT}/{version}/{record['chip']}/{record['name']}"
            upload(s3, scratch / "slices" / record["name"], key)
            chips[record["chip"]] = {
                "url": f"{PUBLIC_BASE}/{key}",
                "sha256": record["sha256"],
                "size": record["size_bytes"],
                "format": "tar.zst",
            }
    component = {
        "id": f"{NAME}/{version}",
        "required": False,
        "asset": {
            "filename": vendor_url.rsplit("/", 1)[-1],
            "size_bytes": size_bytes,
            "sha256": whole_sha,
            "urls": [f"{PUBLIC_BASE}/{whole_key}", vendor_url],
            "slices": chips,
        },
    }
    shard = json.loads(shard_path.read_text())
    shard["components"] = [
        c for c in shard["components"] if c.get("id") != component["id"]
    ]
    shard["components"].append(component)
    text = json.dumps(shard, indent=1) + "\n"
    shard_path.write_text(text)
    index = json.loads(index_path.read_text())
    index["tools"][INDEX_OWNER] = {
        "descriptor": {
            "url": f"shards/{shard_path.name}",
            "sha256": hashlib.sha256(text.encode()).hexdigest(),
            "size_bytes": len(text.encode()),
        },
        "kind_hint": "data",
    }
    index_path.write_text(json.dumps(index, indent=1) + "\n")
    return {"version": version, "chips": len(chips), "whole_key": whole_key}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", required=True, help="local archive or https URL")
    parser.add_argument("--vendor-url", required=True, help="exact URL fbuild resolves")
    parser.add_argument("--shard", type=Path, required=True)
    parser.add_argument("--index", type=Path, required=True)
    parser.add_argument("--credentials", type=Path, required=True)
    args = parser.parse_args()
    result = publish(
        args.source, args.vendor_url, args.shard, args.index, args.credentials
    )
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
