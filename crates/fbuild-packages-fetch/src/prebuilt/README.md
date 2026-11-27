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
| `serve.rs` | `serve`: the serving decision — prebuilt hit, one retry for a transient failure, otherwise `Served::Source` with a printable reason (no error escapes) |
| `serve_tests.rs` | a failing-then-healthy HTTP server: retried once, dead source falls back after exactly two fetches, permanent refusals cost zero requests |
| `pack.rs` | `pack`/`bucket_key`/`release`/`catalog`: the publisher's side — one deterministic archive carrying the required `objects.json` unit manifest, the decision-2 key, and the entry the client reads (never uploads, §8c) |
| `pack_tests.rs` | real objects trees: key shape, determinism across mtimes, a full pack → serve → resolve → fetch → extract round trip, the unit manifest (written, missing object, unsafe id, duplicate, occupied), refusal cases |
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

9. `serve` is the only entry point a build should call, and it never returns
   an error. A transient failure (transport, exhausted sources, integrity,
   archive or IO error, a cancelled extraction) is retried **once** — the
   fetcher keeps its partial bytes so the retry resumes, and each extraction
   attempt stages in its own directory — while a permanent one (an invalid
   identity, a flavor v1 refuses, an archive no descriptor can match, a foreign
   tree already at the path) is refused without spending a request or a second
   attempt. Everything that does not serve returns `Served::Source` with the
   attempts named, so the caller compiles framework from source and prints one
   honest line.

10. `pack` is the publisher's side and is never called by a build. It writes
    one `.tar.zst` whose entry order and metadata (mode from the executable
    bit, uid/gid 0, empty uname/gname, mtime 0) are normalized, so the same
    bytes pack to one digest on any host; it names the archive
    `<product>-<ref>-<chip>.<sha16>.tar.zst` under decision 2's key and emits
    the `Catalog` entry (`flavor fp-<16>`, `provides fbuild-fp:<fingerprint>`)
    that the client above resolves against. A tree holding a symlink, a
    special file, or nothing at all is refused before a byte is written, and a
    non-https `base_url` is refused too — `extract` would reject such an
    archive or the client could never fetch it. The archive is written to a
    `.part` file, re-hashed from disk and only then renamed into place, so a
    killed run leaves nothing publishable. Nothing here uploads: decision 8c
    makes every bucket write a maintainer action. The command that drives it is
    hidden (`fbuild __prebuilt-pack`).

11. A packed tree carries `objects.json` (`schema: 1`, `units: {unit-id:
    object-file}`), because without it nothing is servable: the client hydrates
    a unit only when that map names one of its units. The map is required and
    non-empty, every named object must be a regular file the tree holds, a unit
    id or object name that could address a path outside the tree is refused,
    and a tree that already holds an `objects.json` is refused rather than
    shadowed. The publisher never writes object names by hand: each unit line
    names the source, and the builder derives the file with the same
    `CompilerBase::object_path` the client hydrates to (the name folds the
    source's own path in, so it is not portable — only the id is).

A daemon-wide single-flight is not in this module yet.
