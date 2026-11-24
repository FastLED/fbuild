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

Archive extraction, staging/partial GC, the disk-cache LRU and a daemon-wide
single-flight are not in this module.
