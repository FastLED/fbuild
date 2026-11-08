# Artifact serving — decision document

**Status:** decided, pending implementation · **Feature branch:** `feat/esp-platform-lib-slices` (PR FastLED/fbuild#1890)
**Date:** 2026-11-04 · **Scope:** serving pre-compiled framework libraries + headers from object storage so a user's *first* compile does not build the ESP-IDF / Arduino core from source.

What already exists (the baseline these decisions build on):

- `site/mirror/manifest.json` (`Index`, ~8 KB) → `site/mirror/shards/esp-libs.json` (`Release`, ~19 KB) listing `framework-arduinoespressif32-libs/<version>` components with `asset.urls[]`, `asset.sha256`, and per-chip `slices.<chip> = {url, sha256, size, format}`.
- Six published platform versions (`3.3.5`, `core-3.3.11`, `core-3.3.12`, `5.3`, `3.0.4`, `core-4.0.0-RC1`) × 6–10 chips on DigitalOcean Spaces (`fbuild.sfo3.digitaloceanspaces.com`), 28–94 MB per slice vs 270–390 MB per whole bundle.
- `crates/fbuild-packages-fetch`: `FetchRequest{name, version, url, variant}`, chip normalization (`variants.rs`), sha256 verification, mirror ordering (`downloader_mirrors.rs`), manifest cache with `CACHE_TTL = 24 h` (`downloader_mirror_manifest.rs:47`), whole-bundle fallback.
- Pilot integration tests: `crates/fbuild-packages/tests/it/slice_pilot.rs`.

Known defect that shaped decision 3: a client held a 24-hour-stale shard and therefore could not see newly published slices for a full day.

---

## 1. CLI ergonomics — the opt-out flag

### DECISION

Ship artifact serving **on by default** with exactly one user-facing escape hatch:

```
--no-prebuilt-libs        (+ FBUILD_NO_PREBUILT_LIBS=1)
```

Declared **once, as a global flag** on the top-level `Cli` struct (`#[arg(long = "no-prebuilt-libs", global = true)]`), next to the existing `--no-update-check`, because every package-fetching command needs it: `build`, `install`, `deploy`, `test-emu`, `lib-select`. Documented primarily under `fbuild build`.

During research only, a hidden opt-**in** `--prebuilt-libs` (`hide = true`, `conflicts_with = "no_prebuilt_libs"`) gates the feature; it is **deleted** in the same PR that flips the default, not left as a no-op alias.

### RATIONALE

- fbuild's established vocabulary is `--no-<thing>` for "turn off a default behaviour": `--no-shrink`, `--no-update-check`, `--no-probe-rs`, `--no-graph`, `--no-timestamp`, each with a `FBUILD_NO_*` twin. `--no-prebuilt-libs` is the only candidate that lands in that pattern without inventing a noun.
- Rejected alternatives:
  - `--no-artifacts` — "artifacts" already means *build outputs* in this repo (`--output-dir` "export build artifacts"). Collides head-on.
  - `--no-prebuilt` — reads as "don't use anything prebuilt", which would also implicate toolchains and the PlatformIO package mirror. Too broad a promise to keep.
  - `--no-mirror` — the mirror is also how source packages and toolchains are fetched; disabling prebuilt libs must not disable the download mirror.
  - `--offline-libs` — suggests "work without network", which is false: the source build still downloads the framework source and the toolchain.
- `libs` matches the upstream artifact's own name (`esp32-arduino-libs`, `framework-arduinoespressif32-libs`), so the flag and the thing it disables are spelled the same way.
- One flag, no `--prebuilt-libs=yes|no` tri-state: a boolean opt-out is the smallest surface that covers "the download is broken / I'm air-gapped / I'm bisecting an ABI problem".

### CONCRETE VALUES

| Item | Value |
|---|---|
| Flag | `--no-prebuilt-libs` |
| Help text | `Compile framework libraries from source instead of downloading fbuild's prebuilt per-chip archives (slower first build; use when offline or debugging an ABI problem).` |
| Env var | `FBUILD_NO_PREBUILT_LIBS` (truthy: `1`/`true`/`yes`/`on`, same parser as `FBUILD_NO_UPDATE_CHECK` via `fbuild_core::env_flag::is_set`) |
| Precedence | flag > env var > default (enabled) |
| Scope | `global = true` on `Cli`, consumed in `fbuild-packages-fetch` via an explicit `PrebuiltLibsPolicy` field on `FetchRequest` — never read from the environment deep in the fetch path |
| Research-phase flag | `--prebuilt-libs` (hidden, removed when the default flips) |
| Nightly escape hatch | `--prebuilt-libs-nearest` (see decision 2); off by default, intended for CI only |
| `build_info.json` | records `prebuilt_libs: {used: bool, source: "slice"\|"whole"\|"source", key: "<object key>", sha256: "…"}` so any firmware can be traced to the exact archive it was linked against |

---

## 2. Artifact naming and bucket layout

### DECISION

One provider-agnostic, content-addressed key space, mirrored byte-for-byte across providers. Immutable artifact objects under `v1/`, mutable metadata under `meta/`.

```
v1/<vendor>/<product>/<channel>/<ref>/<chip>/<product>-<ref>-<chip>.<sha16>.tar.zst
v1/<vendor>/<product>/<channel>/<ref>/whole/<product>-<ref>.<sha16>.tar.{xz,zip}
meta/manifest.json
meta/shards/<owner>.json
meta/nightly/<vendor>/<product>/<branch>.json
```

- `v1` is the **layout** version, bumped only on an incompatible key-shape change. It is not a content version.
- `channel` ∈ `release` | `nightly`.
- `release` `ref` is the upstream tag **verbatim** (`3.3.5`, `core-3.3.11`, `4.0.0-RC1`) — immutable forever.
- `nightly` `ref` is `<YYYYMMDD>-<sha12>` (e.g. `20261104-1a2b3c4d5e6f`). **There is no `nightly/latest/` prefix holding object copies**; "latest" is a tiny mutable JSON pointer under `meta/nightly/`.
- Every artifact filename carries the `.<sha16>` prefix of its own sha256. This is currently inconsistent (`3.3.5` slices have it, `core-3.3.11`/`core-3.3.12`/`5.3` do not) — **normalize on publish and backfill**. Content-addressed filenames are what make `immutable` caching safe and make a re-publish impossible to confuse with the original.
- Mirrors replicate the whole key space unchanged, so a mirror is fully described by its `base_url`; the client builds `base_url + "/" + key`. Shards therefore gain `asset.key` and `slices.<chip>.key` alongside the existing absolute `urls`/`url` (which stay as compatibility fallback and remain the only way to express a *vendor origin* URL, which is not a mirror of our key space).

**The stale-master rule.** Commit-addressed artifacts for a moving branch churn faster than any cache can usefully hold, so a nightly is identified by `date+sha` and is **only ever used when its sha matches the sha the build resolved**:

1. Pinned/tagged framework version → `release` channel, exact `ref` match. Staleness is impossible.
2. Floating ref (`master`, `main`, a branch) → resolve HEAD sha, then look for `nightly/<*>-<sha12>`:
   - **exact sha hit** → use it;
   - **no hit** → **fall back to the source build** (decision 5, rung 6) and print one line: `no prebuilt libs for arduino-esp32 master@1a2b3c4 (newest prebuilt 20261103-0f9e8d7, 1 day behind) — building from source`.
3. Never serve a different commit's binaries as if they were HEAD. Prebuilt libs and the headers/ABI the sketch compiles against must come from the same commit; a one-commit skew produces link errors or silent UB that is near-impossible to attribute. "Serve stale with a warning" is therefore rejected *as a default*.
4. CI that explicitly prefers speed over exactness opts in with `--prebuilt-libs-nearest` / `FBUILD_PREBUILT_LIBS_NEAREST=1`: accepts the newest nightly within **7 days**, emits a `WARN` naming both SHAs, and stamps the substitution into `build_info.json`. It is never implied by `-q`/CI detection.

### RATIONALE

- Separating `v1/` (immutable, `max-age=31536000, immutable`) from `meta/` (mutable, `max-age=60`) means one `Cache-Control` rule per prefix and no CDN-edge staleness on artifacts, which is where the bytes are.
- Keys that are identical across providers are the entire mechanism behind decision 5: adding Backblaze B2 or AWS S3 is a sync job plus one line in the index, with no client change and no re-publish.
- Putting the chip in the *path* and in the *filename* makes object listings readable and makes an accidentally cross-wired slice (wrong chip for the directory) visible to a publisher-side checker.
- `date+sha` sorts chronologically in a flat listing while still being exactly attributable, which a bare sha does not, and gives retention a key to sweep on.

### CONCRETE VALUES

| Item | Value |
|---|---|
| Example release key | `v1/espressif/esp32-arduino-libs/release/core-3.3.12/esp32s3/esp32-arduino-libs-core-3.3.12-esp32s3.3e69bee03f836698.tar.zst` |
| Example nightly key | `v1/espressif/esp32-arduino-libs/nightly/20261104-1a2b3c4d5e6f/esp32s3/esp32-arduino-libs-20261104-1a2b3c4d5e6f-esp32s3.<sha16>.tar.zst` |
| Pointer object | `meta/nightly/espressif/esp32-arduino-libs/master.json` → `{"ref":"20261104-1a2b3c4d5e6f","sha":"1a2b3c4d5e6f…","built_at":"2026-11-04T07:12:00Z","chips":["esp32","esp32s3",…]}` |
| Slice format | `tar.zst`, zstd level 19, long-window `--long=27`, one frame |
| Whole-bundle format | upstream's own (`tar.xz` / `zip`), re-hosted unchanged |
| Hash infix | first 16 hex chars of the object's sha256 |
| Chip key set | validated at publish time against fbuild's known chip list; `esp32p4_es` is legitimate, the `esp32s31` key currently in `core-4.0.0-RC1` is a publisher bug and must fail the check |
| Artifact `Cache-Control` | `public, max-age=31536000, immutable` |
| Metadata `Cache-Control` | `public, max-age=60, stale-while-revalidate=300` |
| Nightly cadence | one rotating builder job per vendor/product, 07:00 UTC; HEAD unchanged → same `ref` → idempotent no-op, publishes nothing |
| Nightly retention | last 14 dailies, plus the last nightly of each ISO week for 90 days; older swept |
| Release retention | forever |
| Nearest-nightly window | 7 days (opt-in only) |

---

## 3. Manifest freshness

### DECISION

Replace the single 24-hour TTL with a **three-band policy plus conditional GET**, and add a **miss-triggered forced revalidation**.

| Cache age | Behaviour |
|---|---|
| `< 5 min` | serve cache, zero network |
| `5 min – 24 h` | serve cache **immediately**, revalidate in the background (`If-None-Match` + `If-Modified-Since`); result lands for the next build |
| `> 24 h` | **block** on the conditional GET (existing 10 s per-source budget); on failure serve stale |
| `> 7 d` and unreachable | drop the manifest entirely, fall through to the built-in mirrors (today's behaviour) |

**Miss-triggered revalidation (the fix for the reported bug):** whenever a lookup fails to find the requested `(component, version)` or `slices.<chip>` and the cached document is older than **60 s**, force a synchronous conditional refetch *once per process per owner* before concluding "no slice available". A miss is itself cheap, strong evidence that the document predates a publish. Without this rule a newly published slice is invisible for up to the full TTL no matter how short the TTL is.

Integrity stays hash-based, not signature-based: index carries each shard's `sha256`, shard carries each artifact's `sha256`, everything over TLS. The index additionally gains a monotonically increasing integer `serial`; a document whose `serial` is **lower** than the cached copy's is rejected, which stops a lagging mirror from rolling a client backwards. Signing (minisign over the index) is explicitly **deferred** to v2 and only becomes necessary if we ever serve metadata from a host we do not control.

### RATIONALE

- The documents are ~8 KB + ~19 KB and a `304 Not Modified` is ~300 bytes. At a 5-minute floor and 10 builds/hour, a client spends ≤ 2 conditional GETs per hour — roughly 600 bytes to never again be a day behind. There is no bandwidth argument for a long TTL here.
- Stale-while-revalidate keeps the hot path off the network: a build never waits on the manifest while the cache is < 24 h old, so the shorter TTL costs no wall-clock time.
- The 7-day ceiling and the final fall-through preserve the existing "manifest must never stall a build" invariant.
- `serial` is one integer and closes the one failure mode that TTLs cannot: a mirror that answers `200` with an older document.

### CONCRETE VALUES

| Constant | Value |
|---|---|
| `MANIFEST_FRESH_TTL` | `300 s` |
| `MANIFEST_SWR_WINDOW` | `24 h` (serve-stale + background revalidate) |
| `MANIFEST_STALE_CEILING` | `7 d` |
| `MANIFEST_MISS_REVALIDATE_MIN_AGE` | `60 s` |
| `FETCH_TIMEOUT` | `10 s` per source (unchanged) |
| `MAX_MANIFEST_BYTES` | `8 MiB` (unchanged) |
| Cache sidecar | `<cache>/mirror/<url-hash>.json` + `<url-hash>.meta.json` = `{etag, last_modified, fetched_at, serial}` |
| Conditional headers | `If-None-Match` always when an ETag is stored; `If-Modified-Since` when only `Last-Modified` is known |
| Env overrides | `FBUILD_MIRROR_MANIFEST_URL` (unchanged), `FBUILD_MIRROR_MANIFEST_TTL_SECS` (test/CI only) |
| Process memo | kept, but bypassable once per owner by the miss-triggered path |

---

## 4. Download / stream design

### DECISION

**Single-pass streaming, staging-dir unpack, atomic rename, restart-not-resume below 64 MB, daemon-owned GC.**

Pipeline: `reqwest::bytes_stream()` → sha256 tee → `zstd` streaming decoder → `tar` unpack into a private staging directory. The compressed archive is never materialized on disk in the default path, and decompression overlaps the transfer (the job is network-bound at 28–94 MB).

Because sha256 is only known at end-of-stream, **correctness comes from the rename, not from the lock**:

- unpack into `<cache>/packages/.staging/<name>-<version>-<chip>-<sha16>-<pid>-<rand>/`
- verify sha256 and `size`; on mismatch delete the staging dir and move to the next mirror — no partial content is ever observable
- write `.fbuild-complete` **last** (contents: slice sha256, object key, manifest `serial`, timestamp), `fsync`, then `rename()` the staging dir into `<cache>/packages/<name>/<version>/<chip>/`
- a package directory **without** `.fbuild-complete` is by definition an orphan and is reclaimed, never used

**Resume.** No mid-stream zstd/tar resume — zstd frame state and tar unpack state are not checkpointable without inventing a framing format, and a 28–64 MB slice is 11–26 s at 20 Mbit/s. For objects above `RESUME_THRESHOLD = 64 MB` (real today: `core-3.3.12/esp32p4` 82 MB, `core-4.0.0-RC1/esp32p4_es` 94 MB) use a two-phase path: `HTTP Range` download to `.part` keyed by ETag, then hash and unpack from the file. One extra compressed write buys resumability where it actually matters.

**Concurrency.** Safety is unconditional (separate staging dirs, one rename wins, the loser deletes its own staging and uses the winner's directory). The existing `install_lock.rs` single-flight stays a pure *efficiency* optimization on top — a stale or broken lock can waste a download but can never corrupt a cache entry. Follow-up (file a research issue): move that single-flight into the daemon's in-memory manager to comply with the repo's no-file-locks rule; the in-process `fbuild install` path keeps a process-local mutex.

**GC** is daemon-owned: on daemon startup and every 6 h, on a blocking task that never touches the request path.

### RATIONALE

- Streaming is the whole point of `tar.zst` over `tar.xz`: zstd decompresses at ~1 GB/s, so unpack is free next to the transfer, and peak disk usage is the *extracted* tree instead of extracted + archive.
- Atomic rename makes every other mechanism (locks, resume, GC) optional for correctness, which is the property worth paying for; it also makes the parallel-sketch case trivially safe.
- Resuming a *decompressing* stream would require per-frame checkpoints plus a journal of unpacked inodes — far more machinery than restarting a 15-second download. Gating the complex path at 64 MB confines it to the three largest objects we publish.
- The sentinel file doubles as the cache-key record: it is what lets GC distinguish "installed" from "torn" and lets `build_info.json` be audited after the fact.

### CONCRETE VALUES

| Item | Value |
|---|---|
| Final path | `<cache>/packages/<name>/<version>/<chip>/` |
| Staging path | `<cache>/packages/.staging/<name>-<version>-<chip>-<sha16>-<pid>-<rand>/` |
| Partial path (>64 MB only) | `<cache>/packages/.part/<sha256>.part` + `<sha256>.part.json` = `{url, etag, bytes_committed, started_at}` |
| Completion sentinel | `.fbuild-complete` (written last, fsynced, then rename) |
| `RESUME_THRESHOLD` | `64 MiB` |
| Range retries | 3, backoff `1 s / 4 s / 16 s`, then next mirror; abandon `.part` if the ETag changed |
| No `Accept-Ranges` | no resume — restart from byte 0 |
| Connect timeout | `5 s` · **stall timeout** `30 s` with no bytes received |
| `FBUILD_FETCH_CONCURRENCY` | default `4` concurrent artifact downloads |
| Decompress parallelism | `min(4, available_parallelism())` |
| GC trigger | daemon start + every `6 h`; `fbuild clean cache` / `fbuild purge` run the same sweep with all thresholds = 0 |
| GC rules | staging dir older than `1 h` with no live owner pid → delete · `.part` older than `24 h` → delete · package dir lacking `.fbuild-complete` → delete · never delete anything younger than `1 h` |
| Progress reporting | from `slices.<chip>.size`, so bytes-downloaded is measurable for decision 6 |

---

## 5. Mirror failover and the reference-file indirection

### DECISION

**The index *is* the reference file.** Do not introduce a third document. `meta/manifest.json` gains a top-level ordered `mirrors` array:

```json
"mirrors": [
  {"id": "primary", "base_url": "https://artifacts.fbuild.dev", "weight": 100},
  {"id": "do-sfo3", "base_url": "https://fbuild.sfo3.digitaloceanspaces.com", "weight": 50},
  {"id": "b2",      "base_url": "https://f003.backblazeb2.com/file/fbuild", "weight": 10}
]
```

Clients compile in exactly **two** URLs — the existing `DEFAULT_MANIFEST_URLS` pair (GitHub Pages, then `raw.githubusercontent.com` on `main`). Everything else, including the entire mirror list, is data, so switching DO Spaces → Backblaze → AWS is a publish of a ~200-byte-larger index that every client picks up within the 5-minute TTL (decision 3), with **no fbuild release**.

Belt *and* braces: mirror #1's `base_url` is a stable vanity host (`https://artifacts.fbuild.dev`) fronted by a CNAME we control, so a provider can also be swapped **instantly** at DNS level without touching the index at all. The index list is the reorderable/outage-response mechanism; DNS is the emergency one.

**Validation.** Each `mirrors[]` entry must be `https://`, must have no query/fragment, and its host must match a compiled-in allowlist. A failing *mirror entry* is dropped and the rest are used — deliberately **unlike** the whole-document rejection rule for asset entries, because a mirror list is only a routing hint and content trust never depends on it (every artifact is sha256-checked against the shard, every shard against the index). A malformed *asset* entry still rejects the whole document, as today.

**Fallback ladder for one slice fetch — exact order:**

1. **Local cache hit** — package dir with `.fbuild-complete` and matching sha256. No network.
2. **`mirrors[]`** in index order; one attempt each (plus Range retries for >64 MB objects).
3. **Compiled-in built-in mirror base** (FastLED/fbuild#1806) — covers the case where the index's own mirror list is broken.
4. **Absolute `slices.<chip>.url`** as published in the shard.
5. **Whole vendor bundle** — `asset.urls[]` (our mirror, then vendor origin). Correct but 270–390 MB; already implemented.
6. **Source build** — resolve the framework source package and compile the core, i.e. the pre-#1890 path, identical to `--no-prebuilt-libs`.

Rung 6 must always exist and must be reachable with **zero** reliance on fbuild infrastructure. The command fails only if rung 6 also fails.

Noise discipline: rungs 1–4 are silent (debug-level only). Reaching rung 5 prints one `WARN` naming the rung. Reaching rung 6 prints one line explaining why plus the expected time penalty. A mirror that fails 3 consecutive times in a process is skipped for the remainder of that process and for 10 minutes daemon-wide (circuit breaker).

### RATIONALE

- A separate reference file would add a second document with its own URL, TTL, failure modes and eventual signature, to carry information that fits inside a document the client already fetches on the same code path. Fewer moving parts, one freshness policy.
- Provider-agnostic keys (decision 2) are what make `base_url` sufficient; without them a mirror switch would require re-publishing every absolute URL in every shard.
- The host allowlist stops a compromised or mis-edited index from pointing clients at an arbitrary host; sha256 verification means even a successful redirect cannot deliver different content than published.
- Per-entry (rather than whole-document) rejection for mirrors keeps a typo in a new mirror from disabling artifact serving for everyone — the worst case degrades to the remaining mirrors.

### CONCRETE VALUES

| Item | Value |
|---|---|
| Compiled-in discovery URLs | `https://fastled.github.io/fbuild/mirror/manifest.json`, `https://raw.githubusercontent.com/FastLED/fbuild/main/site/mirror/manifest.json` |
| Primary `base_url` | `https://artifacts.fbuild.dev` (CNAME → current provider) |
| Host allowlist | `artifacts.fbuild.dev`, `*.digitaloceanspaces.com`, `*.backblazeb2.com`, `*.s3.amazonaws.com`, `*.r2.cloudflarestorage.com`, `fastled.github.io`, `github.com`, `objects.githubusercontent.com`, `raw.githubusercontent.com` |
| URL construction | `base_url + "/" + key` (`asset.key` / `slices.<chip>.key`); absolute `url`/`urls[]` used only at rungs 4–5 |
| Per-mirror attempt budget | 1 attempt; 5 s connect, 30 s stall; +3 Range retries above 64 MB |
| Circuit breaker | 3 consecutive failures → skip for the process and 10 min daemon-wide |
| Ladder | cache → `mirrors[]` → built-in base → absolute slice URL → whole bundle → source build |
| Env overrides | `FBUILD_ARTIFACT_MIRRORS` (comma-separated `base_url` list, replaces `mirrors[]`; still allowlist-checked) |

---

## 6. Benchmarking and reporting

### DECISION

Report **one headline number with its control arm** and three supporting series, per `(platform-version, chip, OS)` cell. Raw JSON is the artifact of record; the benchmark page is generated from it.

Headline: `cold_first_compile_wall_s` with prebuilt libs **and** `cold_first_compile_wall_s_source` from a `--no-prebuilt-libs` control run in the same job, reported as absolute seconds plus the speedup ratio. A ratio without the control arm is unfalsifiable, and the control arm is the thing the flag from decision 1 exists to produce.

Supporting metrics: `bytes_downloaded` (and its source-arm counterpart), `artifact_cache_hit_rate` measured as `slices_available / slices_expected` over a nightly matrix sweep (**not** client telemetry — fbuild collects none), `fallback_rung_histogram` from decision 5, and `warm_incremental_s` as a no-regression guard for the non-first-build case.

Gate: a PR that raises `cold_first_compile_wall_s` for the reference cell by more than **10 %** fails the benchmark check. Any rung-6 fallback in a CI build is a hard failure, not a warning — it means artifact serving silently stopped working.

### RATIONALE

- "First compile got faster" is the entire value proposition, so it must be measured against the path it replaces, in the same job, on the same runner, or runner variance swamps the result.
- Cache hit rate measured from the publishing side is both privacy-preserving and more useful: it tells us which `(version, chip)` cells we have *failed to publish*, which is the actual gap.
- The rung histogram is the early-warning signal for mirror rot; without it a provider outage looks like "builds got slower".

### CONCRETE VALUES

| Item | Value |
|---|---|
| Reference cell | `esp32s3` × `framework-arduinoespressif32-libs/core-3.3.12` × `ubuntu-22.04` hosted runner |
| Protocol | 3 runs, report p50; empty `~/.fbuild` **and** empty zccache before each; network warm (one throwaway fetch first) |
| Metrics | `cold_first_compile_wall_s`, `cold_first_compile_wall_s_source`, `speedup_ratio`, `bytes_downloaded`, `bytes_downloaded_source`, `artifact_cache_hit_rate`, `time_to_first_byte_ms` (per mirror), `download_throughput_mbps`, `fallback_rung_histogram`, `warm_incremental_s` |
| Storage | `site/bench/artifact-serving/<YYYY-MM-DD>.json` + regenerated `latest.json` |
| Page | table of all cells + two 90-day sparklines (wall time, bytes) on the existing build benchmark page, rendered from the JSON |
| Cadence | nightly full matrix; reference cell only on every PR |
| Regression gate | `> 10 %` on the reference cell fails; any rung-6 fallback in CI fails |

---

## 7. Required tests

### DECISION

TDD order: every item below is written RED first. Unit tests use local HTTP fixtures (`platformio_registry_fixture.rs` pattern) and `tempfile::TempDir` — never mocks for the HTTP/filesystem boundary, per CLAUDE.md's mocks-vs-fixtures rule. Network-touching integration tests carry `#[ignore = "..."]` with a concrete reason (per the ignored-test policy).

**Unit — `crates/fbuild-packages-fetch` (no network):**

1. **Manifest resolution** — frozen fixture of the current index + `esp-libs` shard; for all 6 published versions assert `slices.esp32s3` resolves to the expected `{url, key, sha256, size}`.
2. **Chip normalization table** — `ESP32-S3` / `esp32s3` / `esp32-s3` → `esp32s3`; `esp32p4_es` preserved; unknown chip → `None` (fall through, **never** a different chip's slice). Regression case for the bogus `esp32s31` key present in `core-4.0.0-RC1`.
3. **Freshness bands** — `< 5 min` → zero requests · `5 min–24 h` → cached body returned *and* a conditional GET issued · `304` → metadata refreshed, body unchanged · `200` → body replaced · `> 7 d` with all sources down → manifest dropped, built-in mirrors used.
4. **Miss-triggered revalidation (the reported bug, RED first)** — cached shard lacking `core-3.3.12`, server has it; one lookup must refetch and succeed inside a single process.
5. **`serial` rollback rejection** — mirror answers `200` with a lower `serial` → document rejected, cached copy retained.
6. **Mirror-entry validation** — `http://`, non-allowlisted host, or relative `base_url` → that entry dropped, remaining entries used; malformed *asset* entry → whole document rejected (existing invariant preserved).
7. **Fallback ladder ordering** — programmable fixture: mirror1 `500`, mirror2 `404`, mirror3 `200` → exactly 3 requests in order; second fetch skips mirror1 (circuit breaker).
8. **sha256 mismatch** — staging dir deleted, next mirror attempted, package dir never created.
9. **Flag/env plumbing** — `--no-prebuilt-libs`, `FBUILD_NO_PREBUILT_LIBS=1`, and both together each disable slice selection; `--help` snapshot pins the spelling and help text.

**Integration — `crates/fbuild-packages/tests/it/`:**

10. **`slice_pilot.rs` expansion** — all 6 versions × `esp32s3`, plus `esp32`, `esp32c3`, `esp32p4` on one version. `#[ignore = "downloads 28-94 MB slices from the artifact mirror"]`.
11. **Abort / resume** — local server advertising `Accept-Ranges` that drops the connection at 60 % of a >64 MB fixture → exactly one Range request at the right offset, correct final hash. Variant without `Accept-Ranges` → restart from 0, still correct.
12. **Concurrency** — 4 concurrent fetchers of the same `(version, chip)` → exactly one package dir, correct content, no staging residue; 4 *different* chips → concurrency cap honoured.
13. **GC** — plant a 2 h-old staging dir, a 48 h-old `.part`, and a package dir lacking `.fbuild-complete` → all three reclaimed; plant a 5-minute-old staging dir → preserved.
14. **Fallback to source** — dead `FBUILD_MIRROR_MANIFEST_URL` + unreachable artifact hosts → build completes from source with the expected single notice and exit 0.
15. **Equivalence (the feature's real correctness guarantee)** — `tests/platform/esp32s3` built via slice vs via `--no-prebuilt-libs`: compare ELF **section sizes and symbol inventory**, not raw bytes (paths/timestamps differ). A divergence here means the slices do not match a source build and the feature is unshippable.
16. **Nightly staleness rule** — floating ref whose HEAD sha has no nightly → source build + notice, exit 0; `--prebuilt-libs-nearest` with a 3-day-old nightly → used, `WARN` emitted, substitution recorded in `build_info.json`; with an 8-day-old nightly → refused.

**Publisher-side (CI, runs on every publish):**

17. **Key-scheme checker** — every object key matches the canonical regex, carries the `.<sha16>` infix matching its real sha256, chip segment ∈ known chip set, filename chip == path chip; index + shards round-trip-validate and `serial` strictly increased.

### RATIONALE

Items 4, 7, 11, 12, 14 and 15 are the ones that would have caught a real defect: 4 is today's bug, 15 is the only test that proves the slices are *correct* rather than merely downloadable, and 17 is what stops a publisher bug like the stray `esp32s31` key from reaching clients at all.

### CONCRETE VALUES

| Item | Value |
|---|---|
| Unit test count | 9 (all offline, all in `fbuild-packages-fetch`) |
| Integration test count | 7 (`crates/fbuild-packages/tests/it/`) |
| CI checks | 1 publisher-side key/schema validator |
| Network-test gating | `#[ignore = "<concrete byte/host reason>"]`, run in `ci-full` |
| Fixture style | local HTTP server + `tempfile::TempDir`; no mocks at the HTTP or filesystem boundary |
| Large-object fixture | synthetic 80 MB incompressible `tar.zst` generated in the test, not a vendored file |

---

## Open follow-ups (file as issues, not resolved here)

1. Move `install_lock.rs`'s directory-based single-flight into the daemon's in-memory manager, to comply with the no-file-locks rule now that correctness no longer depends on it (decision 4).
2. Backfill the `.<sha16>` filename infix on the `core-3.3.11`, `core-3.3.12`, `5.3`, `3.0.4`, `core-4.0.0-RC1` slices and re-publish the shard (decision 2).
3. Republish `core-4.0.0-RC1` without the bogus `esp32s31` chip key, then land the publisher-side checker (decisions 2, 7).
4. Register `artifacts.fbuild.dev` and point it at the current Spaces bucket before `mirrors[]` ships, so the DNS escape hatch exists from day one (decision 5).
