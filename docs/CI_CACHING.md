# CI Caching: Save/Restore fbuild's Disk Cache

**Audience:** FastLED and other downstream projects that want to persist fbuild's
local disk cache across CI runs (GitHub Actions, GitLab CI, Buildkite, Jenkins,
etc.) so a cold runner is effectively warm on the second build.

**Status:** Design doc. Tracks FastLED/fbuild issue #92. Some items below are
marked `TODO: verify` where behavior is not yet fully pinned down from reading
the code — those points should be resolved by experiment or added to the
follow-up list in [Open questions](#open-questions--follow-ups).

## TL;DR

1. Cache the entire **global cache root**:
   `~/.fbuild/prod/cache/` (Unix) or `%USERPROFILE%\.fbuild\prod\cache\` (Windows).
2. Do **not** cache `~/.fbuild/prod/daemon/` — it holds runtime-only state
   (PID file, port file, log file) that must not leak between runners.
3. Make sure no `fbuild-daemon` is alive when the cache is uploaded. The
   easiest way is to stop it at the end of the job (`fbuild daemon stop`) or
   to wait out the 120-second self-eviction timer.
4. Key the cache on `fbuild` version + `platformio.ini` hash + OS/arch.

A drop-in `actions/cache@v4` snippet is in [Worked examples](#worked-examples).

---

## 1. What to cache

### Paths — authoritative source

All path layout is defined in [`crates/fbuild-paths/src/lib.rs`](../crates/fbuild-paths/src/lib.rs).
Resolved relative to the user's home directory:

| Path (Unix)                                     | Path (Windows)                                            | Purpose                          | Cache it? |
|-------------------------------------------------|-----------------------------------------------------------|----------------------------------|-----------|
| `~/.fbuild/prod/cache/`                         | `%USERPROFILE%\.fbuild\prod\cache\`                       | Global cache root (toolchains, packages, libraries) | **Yes** |
| `~/.fbuild/prod/cache/archives/`                | `%USERPROFILE%\.fbuild\prod\cache\archives\`              | Downloaded archives (phase 1)    | Yes |
| `~/.fbuild/prod/cache/installed/`               | `%USERPROFILE%\.fbuild\prod\cache\installed\`             | Extracted content (phase 2)      | Yes |
| `~/.fbuild/prod/cache/index.sqlite`             | `%USERPROFILE%\.fbuild\prod\cache\index.sqlite`           | SQLite cache index (+ `-wal`, `-shm`) | Yes — **see note below** |
| `~/.fbuild/prod/daemon/daemon.port`             | `%USERPROFILE%\.fbuild\prod\daemon\daemon.port`           | Live daemon port file            | **No** |
| `~/.fbuild/prod/daemon/fbuild_daemon.pid`       | `%USERPROFILE%\.fbuild\prod\daemon\fbuild_daemon.pid`     | Live daemon PID file             | **No** |
| `~/.fbuild/prod/daemon/daemon.log`              | `%USERPROFILE%\.fbuild\prod\daemon\daemon.log`            | Daemon log (rotated)             | **No** |
| `~/.fbuild/prod/daemon/daemon_status.json`      | `%USERPROFILE%\.fbuild\prod\daemon\daemon_status.json`    | Live daemon status snapshot      | **No** |
| `<project>/.fbuild/build/`                      | `<project>\.fbuild\build\`                                | Per-project build artifacts      | Optional — see §1.4 |

(These are the paths you get when `FBUILD_DEV_MODE` is unset, i.e. in
production mode. In dev mode the segment `prod` becomes `dev`.)

#### Note on `index.sqlite` and WAL files

The SQLite index is opened in **WAL mode** with `synchronous=NORMAL`
(see [`disk_cache/index.rs`](../crates/fbuild-packages/src/disk_cache/index.rs)).
On a clean daemon shutdown the WAL is checkpointed into `index.sqlite` and the
`-wal`/`-shm` sidecar files disappear. If the cache is uploaded while a daemon
is **running**, those sidecar files will exist and **must** be captured too,
otherwise the restored cache may look empty or corrupted.

**Recommendation:** stop the daemon (`fbuild daemon stop`) before the cache
save step so only `index.sqlite` is left on disk.

### 1.1 What's *in* each phase

- **`archives/<kind>/<stem>/<hash>/<version>/`** — the raw downloaded tarball
  or zip (the thing you'd have downloaded anyway).
- **`installed/<kind>/<stem>/<hash>/<version>/`** — the extracted form that the
  build orchestrator actually consumes.
- **`installed/**/.install_complete`** — sentinel file written after a
  successful extraction. `reconcile()` deletes any installed directory
  missing this sentinel (partial install from a crashed build).
- **`.partial` directories** — in-progress downloads/extractions. Cleaned up
  by reconcile on next daemon start; safe to cache but useless.

`kind` ∈ {`packages`, `toolchains`, `platforms`, `libraries`, `frameworks`}
(see [`disk_cache/paths.rs::Kind`](../crates/fbuild-packages/src/disk_cache/paths.rs)).

### 1.2 Exclusions (DO NOT cache)

From [`fbuild-paths/src/lib.rs`](../crates/fbuild-paths/src/lib.rs) and
[`fbuild-daemon/src/main.rs`](../crates/fbuild-daemon/src/main.rs):

- `daemon/daemon.port` — runtime-only, written on start, deleted on exit.
- `daemon/fbuild_daemon.pid` — runtime-only, points at a PID that won't exist
  on the next runner. Can be misleading (stale PID file is cleaned up at
  daemon startup, but this wastes cache space and time).
- `daemon/daemon.log*` — may contain runner-specific paths and secrets if
  anyone's been careless with env vars.
- `daemon/daemon_status.json` — runtime snapshot.
- Any `*.partial` directory inside `cache/archives` or `cache/installed` —
  incomplete downloads/installs. Harmless to cache but wasteful.

There is **no file-based lock file** to worry about. fbuild deliberately
avoids filesystem locks — all locking is in-memory in the daemon (see
`CLAUDE.md`, "No file-based locks"). Leases are recorded as rows in the
SQLite index, keyed by `(entry_id, holder_pid, holder_nonce)`. When a new
CI runner starts, the old runner's PIDs are gone; `reap_dead_leases()` in
[`disk_cache/gc.rs`](../crates/fbuild-packages/src/disk_cache/gc.rs) sweeps
them on the next GC pass. **This means stale leases from a previous run are
handled automatically** — no manual cleanup needed.

### 1.3 Size expectations

Per-platform rough numbers, from Python fbuild's historical caches:

| Platform family       | Cache size after one cold build | Notes |
|-----------------------|---------------------------------|-------|
| AVR (Uno, Mega, etc.) | ~100–150 MB                     | avr-gcc toolchain + Arduino AVR core |
| ESP32 (Xtensa)        | ~1.5–2.5 GB                     | xtensa-esp-elf toolchain (~1 GB) + ESP-IDF + Arduino core |
| ESP32 (RISC-V: C3/C6) | ~1.5–2 GB                       | riscv32-esp-elf toolchain (~700 MB) + ESP-IDF |
| RP2040                | ~300–400 MB                     | arm-none-eabi-gcc + pico-sdk |
| STM32                 | ~300–400 MB                     | arm-none-eabi-gcc + STM32Cube |
| Teensy                | ~200 MB                         | arm-none-eabi-gcc + teensy core |
| WASM                  | ~500 MB                         | Emscripten |

`TODO: verify` — these numbers are estimates. Measure on a current runner
with `du -sh ~/.fbuild/prod/cache/` at end of a cold build and add a row
to the table or a follow-up bench.

**GitHub Actions has a 10 GB per-repository cache cap** (eviction is LRU
across caches; individual cache entries max out at 10 GB). An all-platform
monorepo cache will easily blow past this, so **split caches per platform**
(see the matrix example in §6).

### 1.4 Project-local build artifacts

`<project>/.fbuild/build/` holds per-environment compiled objects and linked
firmware. It's separate from the global cache root. Caching it gives the
biggest wall-time win (incremental compile avoids even the `.o` rebuild),
but:

- It's stamped with absolute paths from this runner.
- Its validity is tied to the exact source tree — any source change
  re-invalidates most of it.

Start by caching only the global cache (toolchains/packages) — that alone
gives you the warm-start behavior. Layer on per-project build cache later
once the keying story is proven.

---

## 2. Cache key strategy

Recommended cache key composition, highest-specificity first:

```
fbuild-{os}-{arch}-v{fbuild_version}-pio{platformio_ini_hash}-{toolchain_rev}-{source_tree_hash}
```

Concrete example in GitHub Actions syntax:

```yaml
key: fbuild-${{ runner.os }}-${{ runner.arch }}-v${{ env.FBUILD_VERSION }}-${{ hashFiles('platformio.ini', 'boards/**/*.json') }}-${{ hashFiles('src/**/*', 'include/**/*') }}
restore-keys: |
  fbuild-${{ runner.os }}-${{ runner.arch }}-v${{ env.FBUILD_VERSION }}-${{ hashFiles('platformio.ini', 'boards/**/*.json') }}-
  fbuild-${{ runner.os }}-${{ runner.arch }}-v${{ env.FBUILD_VERSION }}-
  fbuild-${{ runner.os }}-${{ runner.arch }}-
```

### Inputs, priority order

1. **fbuild version** — avoids cache poisoning across fbuild upgrades.
   `TODO: verify` how resilient `reconcile_on_open` is across schema
   changes. Today the SQLite schema is at `SCHEMA_VERSION = 1` and
   `migrate()` applies forward migrations (see `index.rs`), so a newer
   fbuild reading an older cache *should* migrate cleanly. A downgrade
   (newer-schema cache read by older fbuild) is not supported and the
   user should bust the cache. Conservative advice: include `fbuild_version`
   in the cache key and skip partial restores across versions.

2. **`platformio.ini` hash + board-JSON hash** — any change here
   potentially changes which toolchains/packages are needed.

3. **Toolchain revision pin** — if you pin a toolchain version in your
   project, include its version string in the key. `TODO: verify` — fbuild
   resolves toolchain URLs at build time from the registry; there's no
   currently-documented way to pin a toolchain for cache-busting purposes
   other than pinning `platformio.ini` values.

4. **OS/arch** — toolchains are OS- and arch-specific. **Never share a
   cache across `runner.os` values.** Same for `runner.arch` (macOS-13
   Intel vs macOS-14 arm64).

5. **Source tree hash** — include this only if you're caching
   `<project>/.fbuild/build/`. Not needed for the global toolchain cache.

### Restore-key fallback chain

`actions/cache` walks `restore-keys` in order, using the first prefix match.
The chain above delivers a partial hit on:

- Same fbuild version, same `platformio.ini`, older source tree
  → full toolchain + install + most objects hit, only changed `.o`
  files rebuild.
- Same fbuild version, different `platformio.ini`
  → toolchain + package *archives* hit, extraction may need to redo
  from archive (fast), some installs may be wrong version and get GC'd.
- Same OS/arch, different fbuild version
  → cache-miss on installs (to be safe); archives may still match by URL.
  Expect a near-cold build.

### Cross-fbuild-version compatibility

`reconcile_on_open` does two phases
(see [`disk_cache/gc.rs::reconcile`](../crates/fbuild-packages/src/disk_cache/gc.rs)):

1. For every index row whose `archive_path`/`installed_path` is now missing
   on disk, null out the column.
2. Walk the filesystem; any leaf directory not referenced by the index is
   deleted as an orphan. `.partial` directories are always deleted.
   Installed directories missing the `.install_complete` sentinel are
   deleted.

**This is always safe — but it does a full walk of `archives/` and
`installed/` on every reconcile.** For a 2 GB ESP32 cache that's a few
hundred ms on a fast SSD, but it's *not free*. Reconcile runs at daemon
startup (see `main.rs`, spawned after bind), so it happens once per CI
job when the daemon starts, not on every build command.

---

## 3. Hermeticity

Can a restored cache produce byte-identical outputs? **Mostly yes, with
caveats.**

### Known deterministic pieces

- Archive `sha256` is recorded in the index on download and re-verified
  on install. Same URL → same bytes → same archive.
- Installed directory layout is a pure function of the archive contents
  (the extractor strips a single top-level dir then unpacks as-is).
- The SQLite index records `last_used_at` and `use_count` for LRU, but
  those are metadata that doesn't affect build outputs.

### Known non-hermetic pieces

- **Object file mtimes.** GCC embeds no mtime by default, but some tools
  (e.g. `ar` with deterministic-off, ranlib) historically stamp indexes.
  fbuild's archiver invocation path should be audited — `TODO: verify`
  whether `ar D` (deterministic) is used everywhere.
- **Absolute paths in debug info.** ESP32 builds in particular embed
  full source paths into `.debug_info`. A restored `.fbuild/build/<env>/`
  from a different runner with different paths will link but produce
  debug info that points at `/home/runner/...` from the *uploading*
  runner. This is cosmetic, not a correctness issue, but it means byte
  equality is off unless runners share paths. Fix via `-fdebug-prefix-map`
  in CI if required — `TODO: verify` whether fbuild sets this today.
- **Response files** (`@argfile`) passed to the compiler/linker may
  contain absolute paths pinned to the build-host. These are regenerated
  each build, so they don't poison the global cache, but they do prevent
  a cached `<project>/.fbuild/build/` from working on a different runner.

### Cross-runner cache poisoning risks

- **macOS Intel vs arm64** — always separate (different toolchain binaries).
- **Ubuntu-22 vs Ubuntu-24** — same arch, but glibc/libc mismatches can
  matter for older host toolchains. Use `${{ runner.os }}` + `runner.image`
  in the key if you care.
- **Different HOME** — the global cache at `~/.fbuild/prod/cache/` is
  keyed by URL hash, not by HOME. Safe across runners.
- **Windows drive letter changes** — unlikely on GitHub-hosted runners,
  but self-hosted setups should stabilize the home drive.

---

## 4. Invalidation

### Budget auto-scaling

From [`disk_cache/budget.rs`](../crates/fbuild-packages/src/disk_cache/budget.rs):

- `archive_budget = min(15 GiB, 5% of total disk)`
- `installed_budget = min(15 GiB, 5% of total disk)`
- `high_watermark = min(30 GiB, 10% of total disk)`
- `low_watermark = 80% of high_watermark`

Example: on a GitHub Actions runner with ~14 GB free on `/` (typical
`ubuntu-latest` after system setup), 5% = ~700 MB each phase budget,
10% = ~1.4 GB combined high watermark. **This is a problem** — a single
ESP32 toolchain (~1 GB) will instantly exceed the high watermark and
trigger eviction.

**Fix:** set the budgets explicitly in CI:

```yaml
env:
  FBUILD_CACHE_ARCHIVE_BUDGET: 8G
  FBUILD_CACHE_INSTALLED_BUDGET: 8G
  FBUILD_CACHE_HIGH_WATERMARK: 16G
```

(Variables parsed by `parse_human_size` — suffixes `K`/`M`/`G`/`T` work,
case-insensitive.) See `disk_cache/budget.rs` for the parser.

### Reconcile cost

`reconcile_on_open` (phase 2 of §2) does a full recursive walk of
`archives/` and `installed/`. On a warm restore of a 2 GB cache, budget
a few hundred ms of I/O at daemon start. It does not run on every
build command — only on daemon startup. For CI this is fine.

`TODO: verify` — benchmark reconcile on a pre-warmed Windows runner
where filesystem walks are comparatively slow.

### Pinning toolchains

There is **no `fbuild cache pin <toolchain>` subcommand today.**
(Grepped `crates/fbuild-cli/src/main.rs` for `pin`: no hits for a
cache subcommand.) The only pinning mechanism in the code is the
RAII `Lease` held for the duration of a build — it prevents GC eviction
*during* the build but releases on drop.

Workaround: bump `FBUILD_CACHE_HIGH_WATERMARK` so GC never fires during
a CI run, which effectively pins everything. Listed as a candidate
follow-up in [Open questions](#open-questions--follow-ups).

---

## 5. Daemon interaction

### Does fbuild have a no-daemon / oneshot mode?

**No, not today** — audited `crates/fbuild-cli/src/main.rs` and
`crates/fbuild-daemon/src/main.rs`. There is no `--foreground`,
`--oneshot`, or `--no-daemon` flag on either binary. Every CLI
command routes through `daemon_client::ensure_daemon_running()`
which either connects to a running daemon or spawns one via
`tokio::process::Command::new("fbuild-daemon")` detached
(see `daemon_client.rs::spawn_daemon_process`).

The daemon self-evicts after **120 seconds** of being idle
(no operations and no serial sessions) — see
[`fbuild-daemon/src/context.rs`](../crates/fbuild-daemon/src/context.rs),
`SELF_EVICTION_TIMEOUT = Duration::from_secs(120)`. Idle timeout
(24 h / 43200 s) is a hard fallback.

### Recommended CI teardown

To guarantee no daemon state leaks between jobs (especially on self-
hosted runners that reuse the filesystem):

```bash
# At end of each CI job, BEFORE the cache-save step:
fbuild daemon stop || true       # graceful shutdown, flushes SQLite WAL
# OR if stop hangs because of stuck state:
fbuild daemon kill --force || true

# Sanity check: no daemon PID file should remain
test ! -f ~/.fbuild/prod/daemon/fbuild_daemon.pid
```

Steps to do this on GitHub Actions are shown in §6.

`TODO: verify` whether `fbuild daemon stop` always checkpoints the
SQLite WAL before exiting. The daemon's shutdown path removes the PID
and port files; the WAL is checkpointed implicitly on SQLite
connection close (rusqlite drops the `Connection` at `DiskCache`
drop). Double-check by inspecting cache size after `fbuild daemon stop`:
`index.sqlite-wal` should be zero bytes or absent.

---

## 6. Worked examples

### 6.1 Drop-in GitHub Actions snippet

Single-platform build:

```yaml
name: build-uno
on: [push, pull_request]
jobs:
  build:
    runs-on: ubuntu-latest
    env:
      FBUILD_VERSION: "0.6.0"   # or derive from package.json / Cargo.toml
      FBUILD_CACHE_ARCHIVE_BUDGET: 8G
      FBUILD_CACHE_INSTALLED_BUDGET: 8G
      FBUILD_CACHE_HIGH_WATERMARK: 16G
    steps:
      - uses: actions/checkout@v4

      - name: Restore fbuild cache
        id: fbuild-cache
        uses: actions/cache@v4
        with:
          path: |
            ~/.fbuild/prod/cache
          key: fbuild-uno-${{ runner.os }}-${{ runner.arch }}-v${{ env.FBUILD_VERSION }}-${{ hashFiles('platformio.ini') }}
          restore-keys: |
            fbuild-uno-${{ runner.os }}-${{ runner.arch }}-v${{ env.FBUILD_VERSION }}-
            fbuild-uno-${{ runner.os }}-${{ runner.arch }}-

      - name: Install fbuild
        run: pip install fbuild==${{ env.FBUILD_VERSION }}

      - name: Build
        run: fbuild build -e uno

      - name: Stop daemon before cache save
        if: always()
        run: |
          fbuild daemon stop || true
          rm -f ~/.fbuild/prod/daemon/*.pid ~/.fbuild/prod/daemon/*.port
```

### 6.2 Matrix example

Cache per-platform to stay under the 10 GB GitHub Actions per-repo cap:

```yaml
jobs:
  build:
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-latest, macos-latest, windows-latest]
        board: [uno, esp32dev, rp2040]
    runs-on: ${{ matrix.os }}
    env:
      FBUILD_VERSION: "0.6.0"
      FBUILD_CACHE_ARCHIVE_BUDGET: 8G
      FBUILD_CACHE_INSTALLED_BUDGET: 8G
      FBUILD_CACHE_HIGH_WATERMARK: 16G
    steps:
      - uses: actions/checkout@v4
      - uses: actions/cache@v4
        with:
          path: |
            ~/.fbuild/prod/cache
          # NOTE: on Windows the path is literally ~/.fbuild/prod/cache
          # actions/cache expands `~` to %USERPROFILE% on Windows runners.
          key: fbuild-${{ matrix.board }}-${{ runner.os }}-${{ runner.arch }}-v${{ env.FBUILD_VERSION }}-${{ hashFiles('platformio.ini') }}
          restore-keys: |
            fbuild-${{ matrix.board }}-${{ runner.os }}-${{ runner.arch }}-v${{ env.FBUILD_VERSION }}-
            fbuild-${{ matrix.board }}-${{ runner.os }}-${{ runner.arch }}-
      - run: pip install fbuild==${{ env.FBUILD_VERSION }}
      - run: fbuild build -e ${{ matrix.board }}
      - if: always()
        run: fbuild daemon stop || true
```

### 6.3 Expected timing

`TBD pending FastLED/fbuild#91` (cache-warm timing measurements).
Coarse expectations based on Python fbuild history:

| Scenario                   | ESP32 first build  | ESP32 warm build   |
|----------------------------|--------------------|--------------------|
| No CI cache (cold runner)  | 3–5 min (download) | —                  |
| CI cache hit (warm runner) | —                  | 20–45 s (link only) |
| CI cache partial hit       | —                  | 60–120 s           |

Numbers are rough. Measure on your runner.

### 6.4 Smoke test: "second run was warm"

Add a job step after the build that asserts warm-start behavior:

```yaml
- name: Assert warm cache
  if: steps.fbuild-cache.outputs.cache-hit == 'true'
  run: |
    # Build should not have re-downloaded the toolchain.
    # Pattern-match on fbuild's log for "downloading" lines.
    if grep -q "downloading " build.log; then
      echo "ERROR: cache hit was reported but a download still occurred"
      exit 1
    fi
```

`TODO: verify` the exact log line fbuild emits for downloads. A more
robust smoke test is to assert total wall time under N seconds for a
no-op build:

```yaml
- name: Warm-build timing assertion
  run: |
    START=$(date +%s)
    fbuild build -e esp32dev
    ELAPSED=$(( $(date +%s) - START ))
    if [ "$ELAPSED" -gt 60 ]; then
      echo "ERROR: warm build took ${ELAPSED}s, expected < 60s"
      exit 1
    fi
```

---

## 7. Related tooling audit

### `fbuild cache export` / `fbuild cache import`?

**Not today.** Audited `crates/fbuild-cli/src/main.rs` for `export`/`import`/
`tarball`/`bundle` — the only `export` in the CLI is `export_bundle` for
emulator artifacts (a build-output feature, not a cache feature).

The daemon exposes `/api/cache/stats` and `/api/cache/gc` via
`fbuild daemon cache-stats` and `fbuild daemon gc`. Bulk export is not
wired up. Listed as a follow-up.

### Two concurrent CI shards sharing a cache

The `lease` table uses `(entry_id, holder_pid, holder_nonce)` as the
primary key and SQLite WAL mode supports concurrent readers + single
writer (see `disk_cache/index.rs` opening `journal_mode=WAL`,
`synchronous=NORMAL`). Two concurrent daemons writing to the **same
`cache/` directory** would share the SQLite index correctly — SQLite
handles the locking. Lease refcounts are per-process via `process_nonce`,
so one shard's drop doesn't release another shard's lease.

Practical concern: **GitHub Actions cache is read-only inside a job**
(it's a tarball restored once, then a separate save at the end). Two
parallel jobs that share a cache key will race on save — whichever
finishes last wins. This is an `actions/cache` limitation, not an
fbuild limitation. Use per-shard cache keys if you need both shards'
caches preserved.

---

## 8. Open questions / follow-ups

### `TODO: verify` items still in this doc

1. Exact per-platform cache sizes after a cold build (§1.3).
2. Whether the toolchain archiver (`ar`) is invoked with `D`
   (deterministic) flags everywhere (§3).
3. Whether fbuild sets `-fdebug-prefix-map` for the build host (§3).
4. Reconcile cost on Windows filesystems (§4).
5. Whether `fbuild daemon stop` always checkpoints the SQLite WAL
   before exiting (§5).
6. The exact log line fbuild emits for a download, for use in smoke
   tests (§6.4).
7. Whether the SQLite migration path gracefully handles a newer-schema
   cache read by an older fbuild (§2). The defensive advice (bust on
   version mismatch) stands regardless.

### Candidate follow-up issues

- **`fbuild cache pin <kind> <url> <version>`** — persistent pin that
  survives GC, for locking a specific toolchain version into the CI
  cache. Today, the only way to prevent eviction is to inflate
  `FBUILD_CACHE_HIGH_WATERMARK`.
- **`fbuild cache export <tarball>` / `fbuild cache import <tarball>`**
  — portable, version-tagged bundles for offline CI and air-gapped
  mirrors. Would also help with `actions/cache` 10 GB cap by letting
  the CI author externalize storage (S3, artifact upload).
- **`fbuild daemon stop --flush`** — explicit flag that guarantees a
  SQLite WAL checkpoint before exit, so the cache save step doesn't
  have to worry about sidecar files.
- **`fbuild --no-daemon build ...`** — one-shot CLI mode that does
  the full build in-process and exits, for CI pipelines that don't
  want a background process at all. Non-trivial because the current
  architecture routes all locking through the daemon.
- **Document a `-fdebug-prefix-map=$PWD=.` default** or expose a flag
  for it, so debug info is portable across runners.
- **Budget defaults tuned for CI** — today the auto-scale logic caps
  at 10% of total disk, which on a 14 GB GitHub-hosted runner is
  below the size of an ESP32 toolchain. A CI-friendly default (or
  `--ci` preset) would avoid the env-var workaround in §4.
