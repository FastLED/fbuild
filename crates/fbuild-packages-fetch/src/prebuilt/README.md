# `prebuilt` — exact-fingerprint prebuilt framework resolution (#1882)

Resolves a local build identity to an opt-in prebuilt framework archive served
from fbuild's mirror bucket. Deliberately **not** part of the Release-only
mirror reader in `downloader::mirror_manifest`: that reader rejects the
`Catalog` kind and its stale-shard fallback does not re-verify against the
current Index descriptor.

| file | contents |
|---|---|
| `mod.rs` | `Query` (exact identity), `Asset`, `Resolution`/`MissReason`, `sha256`/`valid_sha`, `operator_urls` |
| `reader.rs` | `Reader`: Index → per-tool shard, TTL cache, descriptor-verified shard loads |
| `fetch.rs` | `Fetcher`: URL ladder, `Range`/`If-Range` resume, streaming size+SHA checks, process-local single-flight |
| `extract.rs` | `extract`/`extract_async`: single-pass tar.zst extraction, descriptor re-verified while streaming, staging + atomic commit, completion marker |
| `gc.rs` | `sweep_staging`/`sweep_staging_async`: reaps abandoned `.<sha16>.<attempt>.staging` directories left by a killed extraction, with a grace period, a marker guard and a bounded removal count; `sweep_staging_tree`/`sweep_staging_tree_async` is the same policy walked over `{cache}/prebuilt/` |
| `extract.rs` | `extract`/`extract_async`: streaming sha+size verify, hostile-entry refusal, staging + marker commit |
| `extract_tests.rs` | real tar.zst fixtures: reuse, flipped/truncated bodies, traversal, symlink, occupied destination |
| `gc.rs` | `sweep_staging{,_tree}`: reap abandoned `.<sha16>.<n>.staging` directories |
| `gc_tests.rs` | staging fixtures with a synthetic clock: age, marker, symlink and depth bounds |
| `obtain.rs` | `tree_root`/`obtain`: the identity → tree call site (where a tree lives, reuse vs refuse) |
| `obtain_tests.rs` | local HTTP + cache layout: reuse without a request, conflict refused untouched, sweep covers the layout |
| `fetch_tests.rs` | local HTTP fixtures: resume, refused range, 416, corrupt source, over-long/short body, single-flight |
| `gc_tests.rs` | staging debris, marker guard, symlink refusal, foreign names, grace period, removal ceiling, the nested-tree walk (scoped to `{cache}/prebuilt/`, depth- and count-bounded, link-free) |
| `extract_tests.rs` | real tar.zst fixtures: layout, reuse, flipped/truncated body, traversal, absolute path, symlink, occupied dest, size ceiling, wrong format |
| `tests.rs` | local axum fixtures: exact hit, miss, cached/network descriptor checks, operator order |

Rules that decide a hit:

1. The query is an exact identity — `framework`, `arch`, `abi`, `version` and a
   64-hex lowercase fingerprint. A channel name, a version range or a
   fingerprint prefix never authorizes a hit.
2. The Index descriptor pins the shard's exact `size_bytes` **and** SHA-256.
   Both are checked on every read: fresh cache, network fetch, and the stale
   cache used when no source answers.
3. The release must match tool, version, `min_client_version`, a
   `none`/`<arch>`/`<abi>` platform, the `fp-<first 16 hex>` flavor, a
   `fbuild-fp:<fingerprint>` `provides` entry, a 64-hex asset SHA and a
   non-empty https URL list with no multi-part assets.

Anything else is a `Resolution::Miss` and the caller builds from source.

4. `Fetcher` enforces the asset's exact `size_bytes` and SHA-256 while the body
   streams, resumes a partial with `Range` + `If-Range` against the recorded
   ETag/Last-Modified (restarting on a `200`, discarding on a `416`), tries each
   URL twice (resume, then from scratch), and collapses concurrent fetches of one
   digest in this process to a single download.

5. `extract` accepts `.tar.zst` only, reads the archive **once** and recomputes
   the descriptor's `size_bytes` and SHA-256 while decompressing, refuses
   absolute paths, `..`, symlinks, hard links and non-regular entries, bounds
   the decompressed size and entry count, extracts into a staging directory and
   moves it into place only after the marker is written. A destination carrying
   a matching marker is reused without touching the archive; anything else there
   is refused, never merged into.

6. `sweep_staging` removes only directories whose name is exactly
   `.<first 16 hex of the sha256>.<decimal attempt>.staging`, only when they are
   older than a grace period, only when they carry no completion marker, and
   never through a symlink; a sweep removes at most `MAX_REMOVALS` of them and
   reports what it freed. No extraction calls it implicitly, so a live attempt
   is never at risk.

7. `sweep_staging_tree` is the walk, not a second policy: an extracted tree is
   committed wherever its consumer put it, so the abandoned sibling can sit at
   any depth. It sweeps every directory under `{cache}/prebuilt/` (nothing
   outside that root is read), at most `MAX_SCAN_DIRS` directories and
   `MAX_SCAN_DEPTH` levels, at most `MAX_REMOVALS` directories in total, and it
   never enters a symlinked directory. The daemon is the caller — once at
   startup and every 6 h on a blocking task, per decision 4 of
   `docs/design/artifact-serving-decisions.md`
   (`crates/fbuild-daemon/src/bin/entry/prebuilt_gc.rs`).

The disk-cache LRU and a daemon-wide single-flight are not in this module.
