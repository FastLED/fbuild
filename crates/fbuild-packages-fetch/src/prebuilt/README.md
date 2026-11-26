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
| `fetch_tests.rs` | local HTTP fixtures: resume, refused range, 416, corrupt source, over-long/short body, single-flight |
| `extract.rs` | `extract`/`extract_async`: single-pass tar.zst extraction, descriptor re-verified while streaming, staging + atomic commit, completion marker |
| `extract_tests.rs` | real tar.zst fixtures: layout, reuse, flipped/truncated body, traversal, absolute path, symlink, occupied destination, size ceiling, wrong format |
| `gc.rs` | `sweep_staging{,_tree}`: reaps abandoned `.<sha16>.<attempt>.staging` directories left by a killed extraction — grace period, marker guard, bounded removal count, and the nested walk over `{cache}/prebuilt/` |
| `gc_tests.rs` | staging fixtures with a synthetic clock: age, marker, symlink refusal, foreign names, depth and count bounds |
| `lru.rs` | `trees`/`prune`/`touch`: budget-capped LRU eviction of complete trees, oldest-use first |
| `lru_tests.rs` | real trees and markers: candidate rule, root guard, recency order, grace period, removal ceiling, opt-in budget |
| `obtain.rs` | `tree_root`/`obtain`: the identity → tree call site (where a tree lives, reuse vs refuse) |
| `obtain_tests.rs` | local HTTP + cache layout: reuse without a request, conflict refused untouched, sweep covers the layout |
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

8. `prune` evicts a *complete* tree only: a directory below `{cache}/prebuilt/`
   holding a regular `MARKER`, never the prebuilt root itself, never a staging
   directory. Recency is the marker's modification time, which `obtain` bumps on
   every reuse (`touch`), so a tree that keeps being used is never the first to
   go. A tree whose marker is younger than the one-hour grace period is kept and
   counted — a build may be reading it. Eviction is oldest-first, at most
   `MAX_REMOVALS` trees per pass, and stops as soon as the remaining bytes fit
   the budget the operator declared in `FBUILD_PREBUILT_BUDGET`; an unset or
   unparseable budget disables the LRU entirely, so no existing cache is pruned
   by surprise. The daemon runs it on the same 6 h schedule as the sweep.

A daemon-wide single-flight and a source fallback with one retry are not in this
module yet.
