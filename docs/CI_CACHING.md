# CI caching: save/restore design for continuous integration runners

This doc answers: *how do I persist fbuild's cache across CI runs so a cold runner gets a near-warm build?*

It is written for a consumer CI pipeline (e.g., FastLED's GitHub Actions matrix) and describes the contract the cache exposes, the failure modes to avoid, and a drop-in snippet that works out of the box.

For the *local* developer experience see [DEVELOPMENT.md](DEVELOPMENT.md). For *why* the cache exists and its performance targets see [WHY.md](WHY.md). The internal disk-cache implementation lives in `crates/fbuild-packages/src/disk_cache/` with its own [README](../crates/fbuild-packages/src/disk_cache/README.md).

---

## TL;DR — use the composite action

Most consumers should use the [`FastLED/fbuild/.github/actions/setup`](../.github/actions/setup/README.md) composite action — it handles fbuild install, cache restore/save, and env-var wiring in one line:

```yaml
- uses: FastLED/fbuild/.github/actions/setup@main
  with:
    cache-key-extra: ${{ hashFiles('platformio.ini') }}
- run: fbuild build examples/Blink -e esp32dev
```

The action sidesteps the whole `~/.fbuild/*/daemon/` "don't cache ephemeral state" pitfall by redirecting fbuild's cache to `$RUNNER_TEMP/fbuild-cache` via `FBUILD_CACHE_DIR`. See the [action's README](../.github/actions/setup/README.md) for inputs/outputs and a full matrix example.

### Raw snippet (if you don't want the action dependency)

If you skip the composite action, you MUST still bake the fbuild content hash into the cache key — see [Cache-key strategy](#cache-key-strategy) below for why. Minimal version:

```yaml
- name: Resolve fbuild content hash
  id: fbuild-hash
  shell: bash
  run: |
    FBUILD_HASH=$(python - <<'PY'
    import hashlib, importlib.metadata as md
    dist = md.distribution("fbuild")
    record = dist.read_text("RECORD") or f"{dist.version}\n{dist.read_text('METADATA') or ''}"
    print(hashlib.sha256(record.encode('utf-8')).hexdigest()[:16])
    PY
    )
    echo "hash=$FBUILD_HASH" >> "$GITHUB_OUTPUT"

- name: Restore fbuild cache
  uses: actions/cache@v4
  with:
    path: |
      ~/.fbuild/prod/cache
    key: fbuild-${{ runner.os }}-${{ runner.arch }}-${{ steps.fbuild-hash.outputs.hash }}-${{ hashFiles('platformio.ini', '**/boards.txt', 'rust-toolchain.toml') }}-${{ hashFiles('.fbuild-cache-version') }}
    restore-keys: |
      fbuild-${{ runner.os }}-${{ runner.arch }}-${{ steps.fbuild-hash.outputs.hash }}-${{ hashFiles('platformio.ini', '**/boards.txt', 'rust-toolchain.toml') }}-
      fbuild-${{ runner.os }}-${{ runner.arch }}-${{ steps.fbuild-hash.outputs.hash }}-
      fbuild-${{ runner.os }}-${{ runner.arch }}-
```

Adjust `hashFiles(...)` inputs to whatever files actually change the build graph in your project. `.fbuild-cache-version` is a sentinel you bump when you want to force a cache bust.

**Do not cache** `~/.fbuild/*/daemon/` — it's runtime state (port file, PID file, log), and restoring it across runs will make the next client try to connect to a dead daemon.

---

## What the cache holds

| Path | What lives here | Cache in CI? |
|---|---|---|
| `~/.fbuild/prod/cache/archives/` | Downloaded toolchain + framework + library tarballs (pre-extract) | **Yes** |
| `~/.fbuild/prod/cache/installed/` | Extracted, usable toolchains, frameworks, libraries | **Yes** |
| `~/.fbuild/prod/cache/index.sqlite` | LRU index that pairs entries to URLs/versions | **Yes** (must match archives + installed) |
| `<project>/.fbuild/build/` | Per-project build outputs (object files, archives, compile DB, firmware) | **Yes** (the warm-build fast path depends on this) |
| `~/.fbuild/prod/daemon/` | Daemon PID, port, log, status — **ephemeral runtime state** | **No** |

fbuild's cache root defaults to `~/.fbuild/{prod|dev}/cache/` but can be redirected with `FBUILD_CACHE_DIR`. Project-level build outputs default to `<project>/.fbuild/build/` but can be redirected with `FBUILD_BUILD_DIR` — useful on Windows where path lengths matter.

See `crates/fbuild-paths/src/lib.rs` for the authoritative path list.

## Cache-key strategy

The goal is to maximize hit rate without producing wrong output. In priority order, the key should discriminate on:

1. **Runner OS + arch** — `${{ runner.os }}-${{ runner.arch }}`. Toolchains are platform-pinned; sharing across OSes corrupts builds.
2. **fbuild content hash** — **required**, not optional. Key on a content hash of the installed fbuild (e.g., sha256 of the wheel's dist-info `RECORD` file), not just the PyPI version string. A version string does not discriminate against re-released wheels, dev builds, or local installs; cached artifacts can encode fbuild-internal layout (response-file format, path embedding, fingerprint scheme) that changes silently across those. The composite `setup` action computes this hash for you and exports it via the `fbuild-hash` output — consumers who roll their own `actions/cache@v4` should do the equivalent.
3. **Graph inputs** — `platformio.ini`, per-board JSONs, `rust-toolchain.toml`, any `lib_deps`-defining file. A change to these invalidates the warm-build fingerprint anyway, so it's cheap to bake them into the key to avoid carrying obsolete artifacts.
4. **Manual bump** — a `.fbuild-cache-version` sentinel in the repo lets you force-invalidate with a one-line commit when the runtime has rotted for reasons GH Actions can't see.

### Computing the fbuild content hash (if you're not using the composite action)

```yaml
- name: Resolve fbuild content hash
  id: fbuild-hash
  shell: bash
  run: |
    FBUILD_HASH=$(python - <<'PY'
    import hashlib, importlib.metadata as md
    dist = md.distribution("fbuild")
    record = dist.read_text("RECORD") or f"{dist.version}\n{dist.read_text('METADATA') or ''}"
    print(hashlib.sha256(record.encode('utf-8')).hexdigest()[:16])
    PY
    )
    echo "hash=$FBUILD_HASH" >> "$GITHUB_OUTPUT"

- uses: actions/cache@v4
  with:
    path: ~/.fbuild/prod/cache
    key: fbuild-${{ runner.os }}-${{ runner.arch }}-${{ steps.fbuild-hash.outputs.hash }}-${{ hashFiles('platformio.ini') }}
    restore-keys: |
      fbuild-${{ runner.os }}-${{ runner.arch }}-${{ steps.fbuild-hash.outputs.hash }}-
      fbuild-${{ runner.os }}-${{ runner.arch }}-
```

### Restore-key fallback chain

GitHub Actions supports partial hits via `restore-keys`. Use them: a partial hit still restores the toolchains and frameworks (by far the biggest download cost) even when per-project build outputs will be rebuilt.

```yaml
restore-keys: |
  fbuild-${{ runner.os }}-${{ runner.arch }}-${{ hashFiles('platformio.ini') }}-
  fbuild-${{ runner.os }}-${{ runner.arch }}-
```

The index SQLite file is the only thing that MUST match the archives/installed directories it references — and it does, because they're all under one cache path and restored atomically.

## Hermeticity

- **Toolchain binaries, frameworks, and libraries are content-addressed** — they produce bit-for-bit identical intermediate objects given identical inputs, regardless of which runner built them. Safe to share across the matrix.
- **Compile outputs under `.fbuild/build/` are reproducible in file *content* but not always in *mtime***. fbuild's build fingerprint is content-hashed, not mtime-based, so a restored cache does not need mtime preservation from the CI runner — `needs_rebuild` keys on depfile contents and command-hash, not timestamps. This means `actions/cache@v4` restoring without mtime fidelity is fine.
- **Absolute path embedding**: response files (`*.rsp`) under `.fbuild/build/` contain absolute paths to `~/.fbuild/.../installed/...`. On a runner where `$HOME` differs between runs (uncommon but possible), response files become stale. Mitigate by pinning `HOME` to a stable runner path, or by bumping `.fbuild-cache-version` when you change runner images.
- **Debug info**: optimized release builds don't embed source paths beyond what DWARF requires. If you ship debuggable firmware, embedded paths may differ across runners — file-per-runner cache shards if this matters.

## Invalidation

`disk_cache::reconcile_on_open` runs **on daemon startup**, not per-build. On CI where the daemon starts fresh each job, reconcile happens exactly once and is fast (just walks `index.sqlite` + verifies referenced paths exist, removes orphans). No full filesystem rescan per build.

The LRU/GC budget (`crates/fbuild-packages/src/disk_cache/budget.rs`) auto-scales to disk free space. On a 14 GB-free GH Actions runner this lands on a ~10 GB budget — within `actions/cache@v4`'s 10 GB cache-entry limit. If you use a bigger runner, consider pinning the budget with `FBUILD_CACHE_BUDGET=8G` (or whatever you want) to keep the cache entry from growing past what the hosted cache will accept.

### Forcing a rebuild in CI

If you need to bust the cache without deleting anything:

```yaml
env:
  FBUILD_CACHE_VERSION_BUMP: "2026-04-18-purge"
```

Then include `${{ env.FBUILD_CACHE_VERSION_BUMP }}` in your cache key. No code change required.

## Daemon interaction in CI

CI runs are one-shot: runner starts → clone → build → exit. fbuild's daemon is optimized for long-lived interactive sessions, but it works fine one-shot:

- First `fbuild` invocation on a fresh runner spawns the daemon (~200 ms after #91's F2 landed, was 2.2 s).
- Subsequent invocations within the job talk to the already-running daemon.
- When the CI job ends, the runner terminates all processes — the daemon dies with them. `SELF_EVICTION_TIMEOUT` (30 s after #91's F4) is not reached in normal job flow; it's just an insurance policy if the runner hangs for some reason.

**You do not need** `fbuild --no-daemon` on CI. The daemon path is the fast path.

Do **not** cache `~/.fbuild/.../daemon/`. The port and PID files refer to a daemon that no longer exists and will confuse the next run's client.

### Matrix sharding

Matrix jobs sharing one `actions/cache` key will each read the same restore atomically and each write back the entry; GHA cache dedupes on key, so only one write wins. On `disk_cache::lease` acquisition: the sqlite-backed index uses a cross-process advisory lease. Two daemons on the same cache directory (not possible on a single runner, but could happen if two shards mount the same persistent volume) serialize through sqlite — safe but slow. Prefer per-shard caches keyed by shard identifier if you have contention.

## Tooling audit

As of this doc:

- `fbuild cache export <tarball>` / `fbuild cache import <tarball>`: **not implemented**. `actions/cache@v4` handles archive+extract. A native helper would only be needed for non-GHA CI systems.
- `fbuild cache pin <entry>`: **not implemented**. LRU eviction is based on recency; if you need to guarantee a toolchain never evicts on a shared cache, file a follow-up.
- `fbuild cache stats`: yes — `DiskCache::stats()` exposes size and entry counts. Useful for CI debug output.

## Worked example: FastLED's matrix

A FastLED build matrix might look like:

```yaml
strategy:
  matrix:
    os: [ubuntu-latest, macos-latest, windows-latest]
    board: [uno, esp32dev, esp32s3, teensy41]

steps:
  - uses: actions/checkout@v4
  - uses: actions/setup-python@v5
    with: { python-version: '3.12' }
  - run: pip install fbuild

  - name: Restore fbuild cache
    uses: actions/cache@v4
    with:
      path: |
        ~/.fbuild/prod/cache
      key: fbuild-${{ matrix.os }}-${{ matrix.board }}-${{ hashFiles('platformio.ini', 'fbuild-boards/${{ matrix.board }}.json') }}
      restore-keys: |
        fbuild-${{ matrix.os }}-${{ matrix.board }}-
        fbuild-${{ matrix.os }}-

  - name: Build
    run: fbuild build examples/Blink -e ${{ matrix.board }}

  - name: Print cache stats
    run: fbuild cache stats
    if: always()
```

## Expected timings

From #91's profiling run (see `tasks/issue-91-report.md`):

| Scenario | Wall-clock |
|---|---:|
| Warm build, hot daemon, no-op rebuild | 34-71 ms (internal) + HTTP + CLI teardown |
| Cold build, daemon spawn required | +200 ms for spawn + first healthy poll |
| Cache-miss build (full toolchain download + compile) | depends on network; on a good runner, dominated by compile time |

**Note:** the 30 s "warm build stall" originally filed in #91 turned out to be a Windows interactive-shell handle-inheritance bug, not a cache path issue. CI runners with no attached interactive shell do not hit that class of bug.

## Smoke test: confirm your cache is warm

Add a CI step after the build that asserts the second run is cheap:

```yaml
- name: Second build should be a near-no-op
  run: |
    start=$SECONDS
    fbuild build examples/Blink -e ${{ matrix.board }}
    elapsed=$((SECONDS - start))
    test "$elapsed" -lt 10 || { echo "::error::cache restore did not work, second build took ${elapsed}s"; exit 1; }
```

Adjust the threshold to match your project; for a small FastLED sketch, a warm second build is single-digit seconds including fbuild's daemon-spawn cold path.

## See also

- [WHY.md](WHY.md) — performance targets fbuild aims for.
- [architecture/overview.md](architecture/overview.md) — cache lives in `fbuild-packages`.
- `crates/fbuild-packages/src/disk_cache/README.md` — internal structure of the cache directory.
- #91 — warm-path profiling data that informed this doc's timings.
- #92 — issue this doc closes.
