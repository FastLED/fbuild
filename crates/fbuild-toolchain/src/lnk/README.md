# `.lnk` resource pointers

Tiny JSON manifests checked into source control that point at remote binary
blobs. At build time fbuild fetches them, verifies the sha256, caches them
in the shared two-phase disk cache, and materializes them next to where the
`.lnk` would have been (in the build tree, not the source tree).

The intent: keep the source repo small, keep binary assets out of git
history, but have them appear as if they were always there during builds.

## Format (v1)

```json
{
  "v": 1,
  "url": "https://example.com/path/to/asset.bin",
  "sha256": "abcdef0123...64-hex-chars...",
  "size": 1234567,
  "extract": "file"
}
```

| Field | Required | Notes |
|-------|----------|-------|
| `v` | yes | schema version, currently always `1` |
| `url` | yes | http/https only |
| `sha256` | yes | lowercase hex, exactly 64 chars |
| `size` | no | advisory; lets fbuild refuse oversized blobs before fetching |
| `extract` | no | `"file"` (default), `"zip"`, `"tar.gz"` |

`sha256` is **mandatory** by design — reproducible builds and the
content-addressable cache both depend on it. There is no "skip verify"
escape hatch.

## Pipeline

```text
  source tree:                build tree:
    foo.bin.lnk    ─────►       resources/foo.bin
       │                              ▲
       │ scan + parse                 │ hardlink (or copy)
       ▼                              │
    LnkFile                       cached blob
       │                          (under disk_cache, by sha256)
       │ resolve(): cache hit?
       ▼
    DiskCache::lookup(LnkBlobs, url, sha256)
       │
       ├── hit  → lease + return path
       └── miss → download → verify → record → lease + return path
```

Downstream build steps (e.g. esp32's `embed_files` → `objcopy`) consume
the materialized file as if it had been in the source tree all along.

## CLI

```bash
# Fetch every .lnk-referenced blob into the disk cache
fbuild lnk pull [<project_dir>]

# Verify every cached blob matches its sha256 (no network)
fbuild lnk check [<project_dir>]

# One-shot: download a URL, hash it, write a new .lnk
fbuild lnk add <url> [-o <output_path>]
```

## Caching layers

**fbuild side** — uses the existing `DiskCache` with `Kind::LnkBlobs`. Cache
key: `(LnkBlobs, url, sha256)`. The sha256 in the "version" slot guarantees
that flipping the `.lnk`'s sha256 forces a refetch.

- LRU eviction via `disk_cache::gc`
- Lease-aware GC reaping (active builds pin their blobs)
- Storage budget already configured for the rest of the cache applies

**zccache side** — no changes needed. The compile step that consumes the
materialized blob (e.g. `objcopy` invoked by the esp32 orchestrator)
already hashes its inputs as part of the cache key. Because the blob's
on-disk content is byte-identical to its sha256, the cache key changes
whenever the `.lnk`'s sha256 changes. Composition is automatic.

## Integration with `embed_files`

PlatformIO `board_build.embed_files` and `board_build.embed_txtfiles`
entries can mix plain paths with `.lnk` pointers:

```ini
[env:demo]
board_build.embed_files =
    site/dist/index.html.gz       ; plain file in source tree
    assets/large_blob.bin.lnk     ; resolved at build time

board_build.embed_txtfiles =
    config/timezones.json
```

The esp32 orchestrator pre-resolves any `.lnk` entries through
`materialize_lnk_entry` before passing them to `process_embed_files`. The
materialized path is what reaches `objcopy`. The original `.lnk` file is
not visible to downstream tooling.

## Module map

| File | What |
|------|------|
| `format.rs` | `LnkFile` struct, JSON parser, validation |
| `scanner.rs` | `scan_for_lnk(root)` — walk a tree, collect parsed `.lnk`s |
| `resolver.rs` | `resolve(lnk, cache)` — cache hit / miss + download + verify |
| `materialize.rs` | `materialize_one` / `materialize_all` — write blob into build tree |
| `embed.rs` | `expand_lnk_entries` / `materialize_lnk_entry` — glue for `embed_files` |

## FAQ

**Can I use git LFS instead?**
You can — git LFS is orthogonal. But that pulls every blob on every
clone. `.lnk` lets you fetch only what a build actually consumes, with
content-addressable cache sharing across projects on the same machine.

**Why mandatory sha256?**
Because builds without integrity checks aren't reproducible, and a
content-addressable cache without a content key is just a URL cache
(which `disk_cache` already does for toolchain archives).

**What about offline / air-gapped builds?**
`fbuild lnk pull` ahead of going offline. After that, builds use cache
hits and don't touch the network. `fbuild lnk check` validates without
fetching.

**Auth for private URLs?**
Not in v1. Standard environment-based mechanisms (e.g. setting
`HTTPS_PROXY` or providing a token in the URL itself) work today;
first-class token support is a v2 follow-up if needed.

**Cache size?**
Same budget as the rest of the disk cache (auto-scales from free disk
space; configurable through the existing `disk_cache::budget` knobs).

## Related

- [zccache#33](https://github.com/zackees/zccache/issues/33) — adjacent
  pattern: zccache treating runtime DLLs as part of the link artifact
  set so cache hits restore them too.
