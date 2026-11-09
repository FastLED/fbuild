# /// script
# requires-python = ">=3.11"
# dependencies = ["boto3>=1.34", "zstandard>=0.23"]
# ///
"""Publish per-chip slices of one unified ESP toolchain to the mirror.

`fbuild` resolves a toolchain's vendor URL from the platform's toolchain
metadata, looks that exact URL up in the `@esp-toolchain` shard, and downloads
the chip slice when the entry carries one
(`crates/fbuild-packages-fetch/src/variants.rs`). This script performs the
server side of that contract for ONE host package of ONE version:

    uv run --script ci/publish_toolchain_slices.py \
        --metadata ~/.fbuild/prod/cache/toolchains/toolchain-xtensa-esp-elf/metadata/tools.json \
        --toolchain toolchain-xtensa-esp-elf --host linux-amd64 --chips esp32s3 \
        --tree ~/.fbuild/prod/cache/toolchains/toolchain-xtensa-esp-elf/<hash>/xtensa-esp-elf-<v>-x86_64-linux-gnu/xtensa-esp-elf \
        --shard site/mirror/shards/esp-toolchain.json \
        --index site/mirror/manifest.json \
        --credentials ~/.config/do-spaces/credentials.json

Why one invocation per host: a slice holds that host's compilers, so a
linux-amd64 slice is not a linux-arm64 install. The vendor metadata names every
host package (`linux-amd64`, `linux-arm64`, `macos-arm64`, `win32`, ...); each
becomes its own shard component keyed by its own URL, so publishing one host
never claims another. Chips are additive for the same reason: a chip with no
slice keeps the whole-archive fallback, so publishing `esp32s3` first is a
strict improvement for ESP32-S3 and a no-op for every other chip.

Integrity pins come from the metadata, never from a local archive: `sha256` and
`size` are what the vendor publishes for that exact URL, so a mismatched local
tree cannot relabel the asset. `--archive` must be the whole bundle: it is
verified against those pins and mirrored into the bucket, because the committed
manifest contract requires every entry to serve the mirror first and keep the
vendor URL as the fallback. A slice-less chip or an unpublished host therefore
keeps exactly today's behaviour.

Objects land under
`v1/espressif/<toolchain>/release/<version>/<host>/<chip>/`, all `public-read`
so clients need no auth. Files are written only AFTER every upload is verified
by a HEAD size check, so an entry cannot point at an object that is not there.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path

BUCKET = "fbuild"
REGION = "sfo3"
ENDPOINT = f"https://{REGION}.digitaloceanspaces.com"
PUBLIC_BASE = f"https://{BUCKET}.{REGION}.digitaloceanspaces.com"
SCHEMA = "https://zackees.github.io/manifest.json/v1/manifest.schema.json"
TOOL = "fbuild-download-mirror/esp-toolchain"
OWNER = "@esp-toolchain"
SHARD_NAME = "esp-toolchain.json"
KEY_ROOT = "v1/espressif"
MAX_INDEX_BYTES = 8 * 1024 * 1024
TOKEN = re.compile(r"^[0-9A-Za-z._+-]+$")
SHA256 = re.compile(r"^[0-9a-f]{64}$")
# Every host key `fbuild` itself can resolve. `win32` is listed by the vendor
# metadata but is DEAD for fbuild: `detect_platform()` returns `win64` on every
# Windows host (32-bit Windows is not a target), so a `win32` slice would be
# downloaded by nobody -- `win64` is the key that wins bytes on Windows. The
# parity test `test_publish_toolchain_slices.HostKeyParityTests` reads
# `detect_platform()` and fails if a key it returns is missing here, so this
# tuple can never fall behind the client again (#1956).
HOST_KEYS = (
    "linux-amd64",
    "linux-arm64",
    "linux-armel",
    "linux-armhf",
    "linux-i686",
    "macos",
    "macos-arm64",
    "win32",
    "win64",
)


def sha256_hex(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def file_sha256(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def _version_token(name: str) -> str:
    """`esp-14.2.0_20260121` -> `14.2.0_20260121`, and it must stay a path token."""
    token = name[4:] if name.startswith("esp-") else name
    if not TOKEN.match(token):
        raise ValueError(f"version {name!r} is not a safe path token")
    return token


def load_hosts(
    metadata: dict, toolchain: str, version: str | None = None
) -> tuple[str, dict]:
    """The version entry's per-host `{url, sha256, size}` map."""
    tools = [
        tool for tool in metadata.get("tools", []) if tool.get("name") == toolchain
    ]
    if len(tools) != 1:
        raise ValueError(f"metadata lists {len(tools)} tools named {toolchain!r}")
    versions = tools[0].get("versions") or []
    if version is None:
        chosen = [entry for entry in versions if entry.get("status") == "recommended"]
        if len(chosen) != 1:
            raise ValueError(
                f"{toolchain}: {len(chosen)} recommended versions; pass --version-name"
            )
    else:
        chosen = [entry for entry in versions if entry.get("name") == version]
    if len(chosen) != 1:
        raise ValueError(f"{toolchain}: version {version!r} not found exactly once")
    entry = chosen[0]
    hosts = {
        key: value
        for key, value in entry.items()
        if key in HOST_KEYS and isinstance(value, dict) and value.get("url")
    }
    if not hosts:
        raise ValueError(
            f"{toolchain} {entry.get('name')}: metadata lists no host packages"
        )
    return _version_token(str(entry["name"])), hosts


def validate_host(hosts: dict, host: str) -> dict:
    """Refuse anything that cannot be pinned; a missing pin is a hard error."""
    if host not in hosts:
        raise ValueError(
            f"host {host!r} not in metadata (have: {', '.join(sorted(hosts))})"
        )
    entry = hosts[host]
    if not SHA256.match(str(entry.get("sha256", ""))):
        raise ValueError(f"{host}: metadata sha256 is not 64 lowercase hex digits")
    size = entry.get("size")
    if not isinstance(size, int) or size <= 0:
        raise ValueError(f"{host}: metadata size is not a positive integer")
    return entry


def slice_key(version: str, host: str, chip: str, record: dict, toolchain: str) -> str:
    """Bucket key for one slice; the digest in the name makes it immutable."""
    stem = Path(record["name"]).name.removesuffix(".tar.zst")
    if not TOKEN.match(chip) or not TOKEN.match(host) or not TOKEN.match(stem):
        raise ValueError(f"unsafe key component: {chip!r} / {host!r} / {stem!r}")
    return f"{KEY_ROOT}/{toolchain}/release/{version}/{host}/{chip}/{stem}.{record['sha256'][:16]}.tar.zst"


def whole_key(version: str, host: str, filename: str, toolchain: str) -> str:
    """Bucket key for the whole bundle, named after the VENDOR asset.

    Never after the operator's local file name: the shard's `asset.filename`
    comes from the vendor URL, so a bundle uploaded under an alias
    (`arm64.tar.xz`) leaves the listed filename pointing at a second copy of the
    same bytes that nothing names -- and a re-publish under another alias adds
    yet another object for the same host.
    """
    name = Path(filename).name
    if not TOKEN.match(name):
        raise ValueError(f"unsafe whole-archive filename: {filename!r}")
    return f"{KEY_ROOT}/{toolchain}/release/{version}/{host}/whole/{name}"


def build_component(
    toolchain: str,
    version: str,
    host: str,
    entry: dict,
    slices: dict[str, dict],
    whole_url: str,
) -> dict:
    """One shard component: the mirrored asset plus its per-chip slices.

    The bucket copy is not optional: the committed-manifest contract (and
    `committed_manifest_is_accepted_and_prefers_the_bucket`) requires every
    published entry to serve from the mirror first, with the vendor URL kept as
    the fallback. A vendor-first entry would also make the fallback for every
    chip without a slice depend on GitHub being reachable.
    """
    if not slices:
        raise ValueError("refusing to publish a component with no slices")
    if not whole_url:
        raise ValueError("the whole archive must be mirrored before it is listed")
    urls = [whole_url, entry["url"]]
    return {
        "id": f"{toolchain}/{version}/{host}",
        "required": False,
        "asset": {
            "filename": Path(str(entry["url"])).name,
            "size_bytes": entry["size"],
            "sha256": entry["sha256"],
            "urls": urls,
            "slices": slices,
        },
    }


def upsert_component(shard: dict, component: dict) -> dict:
    """Replace the component with the same id, keeping chips an earlier run
    published.

    Chips are published one invocation at a time (one chunk of CPU each), so a
    replace-everything upsert would make `--chips esp32` silently retract the
    `esp32s3` slice the previous run shipped. A chip this run DOES list wins,
    because re-slicing changed bytes.
    """
    existing = next(
        (c for c in shard.get("components", []) if c.get("id") == component["id"]),
        None,
    )
    merged = dict(component)
    if existing is not None:
        asset = dict(component["asset"])
        chips = {
            **existing.get("asset", {}).get("slices", {}),
            **component["asset"].get("slices", {}),
        }
        if chips:
            asset["slices"] = chips
        merged["asset"] = asset
    components = [
        c for c in shard.get("components", []) if c.get("id") != component["id"]
    ]
    components.append(merged)
    shard["components"] = components
    return shard


def register(shard_path: Path, index_path: Path, component: dict) -> dict:
    """Write the shard and point the Index owner at exactly those bytes."""
    shard = (
        json.loads(shard_path.read_text())
        if shard_path.exists()
        else {
            "$schema": SCHEMA,
            "kind": "Release",
            "schema_version": 1,
            "tool": TOOL,
            "version": "1",
        }
    )
    shard["tool"] = TOOL
    text = json.dumps(upsert_component(shard, component), indent=1) + "\n"
    payload = text.encode()
    if len(payload) > MAX_INDEX_BYTES:
        raise ValueError(f"shard would be {len(payload)} bytes, over the index limit")
    index = json.loads(index_path.read_text())
    if index.get("kind") != "Index":
        raise ValueError(f"{index_path} is not an Index document")
    shard_path.write_text(text)
    index["tools"][OWNER] = {
        "descriptor": {
            "url": f"shards/{shard_path.name}",
            "sha256": sha256_hex(payload),
            "size_bytes": len(payload),
        },
        "kind_hint": "data",
    }
    index_path.write_text(json.dumps(index, indent=1) + "\n")
    return {
        "shard": str(shard_path),
        "sha256": sha256_hex(payload),
        "size_bytes": len(payload),
    }


def check(shard_path: Path, index_path: Path) -> dict:
    """Assert the Index still describes the shard that is checked in.

    The Index is a generated document, so a regeneration that does not know
    about this owner drops `@esp-toolchain` and every client silently goes back
    to whole archives. That failure is invisible in the shard itself, so it is
    checked here instead: the descriptor must hash these exact bytes.
    """
    if not shard_path.exists():
        raise ValueError(f"{shard_path} does not exist")
    payload = shard_path.read_bytes()
    shard = json.loads(payload)
    components = [
        c for c in shard.get("components", []) if c.get("asset", {}).get("slices")
    ]
    if not components:
        raise ValueError(f"{shard_path} lists no component with slices")
    index = json.loads(index_path.read_text())
    descriptor = index.get("tools", {}).get(OWNER, {}).get("descriptor")
    if descriptor is None:
        raise ValueError(f"{index_path} does not list {OWNER}")
    expected = {"sha256": sha256_hex(payload), "size_bytes": len(payload)}
    actual = {key: descriptor.get(key) for key in expected}
    if actual != expected:
        raise ValueError(f"{OWNER} descriptor {actual} does not describe {shard_path}")
    return {
        "component": components[0]["id"],
        "chips": sorted(components[0]["asset"]["slices"]),
        **expected,
    }


def upload(s3, path: Path, key: str) -> None:
    """Upload one object and prove a client can see exactly those bytes."""
    s3.upload_file(str(path), BUCKET, key)
    s3.put_object_acl(Bucket=BUCKET, Key=key, ACL="public-read")
    head = s3.head_object(Bucket=BUCKET, Key=key)
    size = head["ContentLength"] if isinstance(head, dict) else head.content_length
    if size != path.stat().st_size:
        raise ValueError(
            f"{key}: uploaded {size} bytes, expected {path.stat().st_size}"
        )


def client(credentials: Path):
    """boto3 is imported here so this module stays importable without it."""
    import boto3

    creds = json.loads(credentials.read_text())
    return boto3.client(
        "s3",
        region_name=REGION,
        endpoint_url=ENDPOINT,
        aws_access_key_id=creds["access_key"],
        aws_secret_access_key=creds["secret_key"],
    )


def slice_tree(tree: Path, chips: tuple[str, ...], out_dir: Path, level: int) -> dict:
    """Run the slicer as a subprocess so its PEP 723 deps stay independent."""
    script = Path(__file__).with_name("slice_toolchain.py")
    argv = [
        sys.executable,
        str(script),
        str(tree),
        "--out-dir",
        str(out_dir),
        "--level",
        str(level),
    ]
    if chips:
        argv += ["--chips", ",".join(chips)]
    result = subprocess.run(argv, check=True, capture_output=True, text=True)
    return json.loads(result.stdout)


def publish(
    metadata: dict,
    toolchain: str,
    host: str,
    shard_path: Path,
    index_path: Path,
    s3,
    *,
    version_name: str | None = None,
    slices: dict | None = None,
    slices_dir: Path | None = None,
    archive: Path | None = None,
    tree: Path | None = None,
    chips: tuple[str, ...] = (),
    level: int = 19,
    run_slicer=slice_tree,
) -> dict:
    """Upload the slices, verify them, then make them resolvable."""
    version, hosts = load_hosts(metadata, toolchain, version_name)
    entry = validate_host(hosts, host)
    if slices is None:
        if tree is None or slices_dir is None:
            raise ValueError("--tree and a slice output directory are required")
        slices = run_slicer(tree, chips, slices_dir, level)
    if slices_dir is None:
        raise ValueError("--slices-dir is required so the uploaded files can be found")
    published = {
        record["chip"]: _uploaded(s3, slices_dir, record, version, host, toolchain)
        for record in slices["slices"]
    }
    if archive is None:
        raise ValueError(
            "--archive is required: the whole bundle must be mirrored first"
        )
    whole_url = _mirror_whole(s3, archive, entry, version, host, toolchain)
    component = build_component(toolchain, version, host, entry, published, whole_url)
    registered = register(shard_path, index_path, component)
    return {
        "component": component["id"],
        "version": version,
        "host": host,
        "chips": sorted(published),
        "urls": component["asset"]["urls"],
        "shard": registered,
    }


def _uploaded(
    s3, slices_dir: Path, record: dict, version: str, host: str, toolchain: str
) -> dict:
    """Upload one slice and describe it the way the shard must list it."""
    key = slice_key(version, host, record["chip"], record, toolchain)
    upload(s3, slices_dir / record["name"], key)
    return {
        "url": f"{PUBLIC_BASE}/{key}",
        "sha256": record["sha256"],
        "size": record["size_bytes"],
        "format": "tar.zst",
    }


def _mirror_whole(
    s3, archive: Path | None, entry: dict, version: str, host: str, toolchain: str
) -> str | None:
    """Mirror the vendor archive, refusing bytes the metadata does not pin."""
    if archive is None:
        return None
    if (
        file_sha256(archive) != entry["sha256"]
        or archive.stat().st_size != entry["size"]
    ):
        raise ValueError(f"{archive} does not match the metadata pins for {host}")
    key = whole_key(version, host, Path(str(entry["url"])).name, toolchain)
    upload(s3, archive, key)
    return f"{PUBLIC_BASE}/{key}"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--metadata", type=Path, help="pioarduino toolchain metadata JSON"
    )
    parser.add_argument("--toolchain", help="e.g. toolchain-xtensa-esp-elf")
    parser.add_argument("--host", help="metadata host key, e.g. linux-amd64")
    parser.add_argument(
        "--version-name",
        default=None,
        help="metadata version name, default: recommended",
    )
    parser.add_argument(
        "--tree", type=Path, default=None, help="extracted toolchain root for --host"
    )
    parser.add_argument(
        "--chips", default="", help="comma-separated, default: every chip in the tree"
    )
    parser.add_argument(
        "--slices-dir", type=Path, default=None, help="reuse an existing slicer output"
    )
    parser.add_argument(
        "--archive",
        type=Path,
        default=None,
        help="the vendor archive to mirror (required unless --check)",
    )
    parser.add_argument("--level", type=int, default=19)
    parser.add_argument("--shard", type=Path, required=True)
    parser.add_argument("--index", type=Path, required=True)
    parser.add_argument(
        "--credentials", type=Path, help="do-spaces JSON with access_key/secret_key"
    )
    parser.add_argument(
        "--check", action="store_true", help="verify the committed shard and Index only"
    )
    args = parser.parse_args()
    if args.check:
        print(json.dumps(check(args.shard, args.index), indent=2))
        return
    missing = [
        name
        for name in ("metadata", "toolchain", "host", "credentials", "archive")
        if getattr(args, name) is None
    ]
    if missing:
        parser.error(f"missing: {', '.join('--' + name for name in missing)}")
    chips = tuple(c.strip() for c in args.chips.split(",") if c.strip())
    if args.slices_dir is None and args.tree is None:
        parser.error("--tree is required unless --slices-dir is given")
    out_dir = args.slices_dir
    if out_dir is None and args.tree is not None:
        out_dir = args.tree.parent / "slices"
    slices = None
    if args.slices_dir is not None:
        slices = json.loads((args.slices_dir / "slices.json").read_text())
    result = publish(
        json.loads(args.metadata.read_text()),
        args.toolchain,
        args.host,
        args.shard,
        args.index,
        client(args.credentials),
        version_name=args.version_name,
        slices=slices,
        slices_dir=out_dir,
        archive=args.archive,
        tree=args.tree,
        chips=chips,
        level=args.level,
    )
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
