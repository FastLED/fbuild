# Warm-Pass Build Performance Investigation (FastLED/fbuild#91)

Investigation into why warm-pass builds for FastLED sketches report ~30s per
sketch when the effective compile work is reported as <1s.

This document is the outcome of issue #91 — investigation + instrumentation
only. No fixes are applied here; stalls are prioritized for follow-up issues.

## TL;DR

- Steady-state warm builds on small test projects are already fast
  (~70–250 ms wall-clock for both `tests/platform/uno` and
  `tests/platform/esp32dev`).
- **The dominant stall is the *first* warm build after a daemon (re)start
  on ESP32**: with the on-disk cache fully populated and the fast-path
  fingerprint file present, a single ESP32 build consumes **~68 s** before
  any object file is recompiled.
- All ~68 s is spent inside the ESP32 orchestrator *after* the fast-path
  fingerprint check returns "miss" on the first cold-daemon call — in the
  library discovery, include-path computation, framework library
  compilation-scan, and per-source staleness walks.
- Secondary stall: the second warm build takes ~1.3 s even after the
  fast-path fingerprint is re-populated (first hit after a miss), because
  the second pass still does a full `collect_fast_path_watches` traversal
  and one round of zccache fingerprint verification.

Recommended follow-up: split the fast-path fingerprint into *in-memory* +
on-disk layers so repeated builds against the same daemon never re-touch
thousands of watched files.

## Methodology

### Environment

- Platform: Windows 10 Pro, x86_64-pc-windows-msvc
- Binaries: release build of `fbuild.exe` + `fbuild-daemon.exe` from this
  branch
- Dev mode: `FBUILD_DEV_MODE=1` (port 8865, cache at `~/.fbuild/dev/`)
- Daemon: spawned fresh for this experiment
- Commit: see the `docs(perf): warm-pass build investigation` commit on
  worktree branch `worktree-agent-a4ab8496`

### Projects

1. **`tests/platform/uno`** — AVR Arduino Uno, an 8-line `.ino` that calls
   `pinMode`, `Serial.begin`, `digitalWrite`, `Serial.println`. Smallest
   viable baseline (1 sketch source, 25 core files, 1 variant file).
2. **`tests/platform/esp32dev`** — ESP32 Dev Module (Xtensa LX6), an 8-line
   `.ino` using the Arduino framework + pioarduino platform. Closest small
   analogue to FastLED's ESP32 CI path (58 core files, Arduino framework
   built-in libraries).

`esp32dev` is the better proxy for the FastLED issue because FastLED's
warm-pass gripe is ESP32-shaped.

### Instrumentation

An env-gated (`FBUILD_PERF_LOG=1`) per-phase wall-clock timer was added to
`fbuild-build`. It emits one summary line per scope on drop and does nothing
when the env var is unset. Three scopes are wired:

- **`cli-build`** (CLI side, in `crates/fbuild-cli/src/main.rs::run_build`):
  `daemon-handshake`, `server-roundtrip`, `total`
- **`daemon-handler`** (in
  `crates/fbuild-daemon/src/handlers/operations.rs::build`):
  `lock-wait`, `build-wallclock`, `total`
- **`avr-orchestrator`** / **`esp32-orchestrator`** /
  **`pipeline`** (in `crates/fbuild-build/src/perf_log.rs` + wiring in
  `avr/orchestrator.rs`, `esp32/orchestrator.rs`, `pipeline.rs`):
  `config-parse`, `board-load`, `build-dirs`, `flag-collect`,
  `toolchain-ensure`, `framework-ensure`, `source-scan`,
  `pioarduino-resolve`, `fp-watches-collect`, `fast-path-check`,
  `zccache-discover`, plus the pipeline compile phases
  (`compile-core`, `compile-variant`, `compile-sketch`,
  `compile-local-libs`, `project-as-lib`, `compile-db`, `link`).

Summaries are written via `tracing::info!` under targets
`fbuild_build::perf_log`, `fbuild_cli::perf_log`, and
`fbuild_daemon::perf_log`, and also mirrored to stderr so they always appear
regardless of subscriber configuration. The daemon log at
`~/.fbuild/dev/daemon/daemon.log` contains the daemon-side summaries.

### Experiment

For both projects:

1. Stop daemon (`fbuild daemon stop`, plus `taskkill` for any stuck PIDs).
2. Cold build once to fully populate the on-disk cache + build artifacts.
3. Run 5 warm builds back-to-back under `FBUILD_PERF_LOG=1`. Record CLI
   wall-clock (`time.time()` deltas around the subprocess) + CLI timer
   summary + daemon timer summary for each run.
4. Take the median per phase.

Wall-clock outside the perf-log timers was measured with a Python
`time.time()` delta around each subprocess, because bash's `time` + `tee`
gave wildly inflated numbers in early runs (reporting 2 minutes for a
0.25 s invocation).

## Results

### AVR — `tests/platform/uno`

Cold build: **42.7 s** (mostly avr-gcc toolchain download on the first run).

Warm-pass, median across 5 runs:

| Phase (scope)                      | Median   | % of wall |
|------------------------------------|---------:|----------:|
| Wall-clock (outside fbuild)        | 249 ms   | 100.0%    |
| CLI `total`                        | 122 ms   |  49.0%    |
| CLI `daemon-handshake`             |   0 ms   |   0.0%    |
| CLI `server-roundtrip`             | 121 ms   |  48.6%    |
| Daemon-handler `build-wallclock`   | 120 ms   |  48.2%    |
| Daemon-handler `lock-wait`         |   0 ms   |   0.0%    |
| AVR orchestrator `total`           | 120 ms   |  48.2%    |
|   ├─ `config-parse`                |   0 ms   |   0.0%    |
|   ├─ `board-load`                  |   0 ms   |   0.0%    |
|   ├─ `build-dirs`                  |   0 ms   |   0.0%    |
|   ├─ `flag-collect`                |   0 ms   |   0.0%    |
|   ├─ `toolchain-ensure`            |   6 ms   |   2.4%    |
|   ├─ `framework-ensure`            |  10 ms   |   4.0%    |
|   ├─ `source-scan`                 |   3 ms   |   1.2%    |
|   └─ pipeline work                 |  65 ms   |  26.1%    |
| Pipeline `total`                   |  66 ms   |  26.5%    |
|   ├─ `compile-core`                |  10 ms   |   4.0%    |
|   ├─ `compile-variant`             |   0 ms   |   0.0%    |
|   ├─ `compile-sketch`              |   0 ms   |   0.0%    |
|   ├─ `compile-local-libs`          |   0 ms   |   0.0%    |
|   ├─ `project-as-lib`              |   0 ms   |   0.0%    |
|   ├─ `compile-db`                  |   0 ms   |   0.0%    |
|   └─ `link`                        |  55 ms   |  22.1%    |
| Wall − CLI total (bash overhead)   | 127 ms   |  51.0%    |

The 127 ms of "outside" is process spawn + Windows `CreateProcess` + Rust
runtime init for the CLI — everything before `run_build` starts its timer.
Not a target for fixes here.

### ESP32 — `tests/platform/esp32dev`

Cold build: **857 s** (~14 min; includes SDK + toolchain + framework
download, full core+SDK compile, final link).

Warm-pass, **five consecutive runs**:

| Run | Wall      | CLI total | Daemon `build-wallclock` | Orchestrator `total` | `fast-path-check` | Notes                                              |
|-----|----------:|----------:|-------------------------:|---------------------:|------------------:|----------------------------------------------------|
| 1   | **72.1 s**| **69.8 s**| **67.7 s**               | **67.7 s**           |   0 ms            | Fresh daemon. Fingerprint missed, full scan path. |
| 2   |  1.56 s   |  1.32 s   |  1.32 s                  |  1.32 s              |  51 ms            | Fingerprint hit, but `compile_db_is_current` +    |
|     |           |           |                          |                      |                   |   fp-watches walk still run.                      |
| 3   |  259 ms   |   67 ms   |   64 ms                  |   64 ms              |  35 ms            | Steady state.                                     |
| 4   |  296 ms   |   74 ms   |   71 ms                  |   70 ms              |  41 ms            | Steady state.                                     |
| 5   |  281 ms   |   81 ms   |   79 ms                  |   79 ms              |  45 ms            | Steady state.                                     |

Median for the steady state (runs 3–5): **~280 ms wall / ~74 ms CLI total /
~70 ms orchestrator total**.

**The first warm build after a daemon restart costs 67.7 s.** This is the
dominant stall and the most plausible source of the FastLED "30 s per sketch"
report: in CI, the daemon often starts fresh; in a FastLED repo with dozens
of ESP32 examples, every sketch's first build after daemon startup would hit
this 67.7 s path.

All measured sub-phases of the ESP32 orchestrator sum to ~29 ms on run 1:

```
[perf-log esp32-orchestrator] zccache-discover=0 ms, config-parse=0 ms,
  board-load=8 ms, build-dirs=0 ms, flag-collect=0 ms, pioarduino-resolve=21 ms,
  fp-watches-collect=0 ms, fast-path-check=0 ms, total=67718 ms
```

That means **~67.69 s are spent between the fast-path-check returning
"miss" and the end of the orchestrator**, in code that the current
instrumentation does *not* yet break out — the sections described by
`crates/fbuild-build/src/esp32/orchestrator.rs`:

1. Library dependency resolution (`ensure_libraries_sync`), including
   re-scanning library trees + LDF-style include probing.
2. Framework built-in library compilation scan (`fw_libs` — WiFi, FS,
   SPIFFS, Network, BluetoothSerial, etc.), which iterates all
   ~50 framework library subdirectories and per-file staleness-checks
   every `.c`/`.cpp` in each.
3. Include-path assembly (ESP32 typically has 305+ include dirs).
4. Per-source `needs_rebuild_with_signature` calls, each of which:
   - stats the object file,
   - reads the `.cmdhash` stamp,
   - parses the `.d` depfile,
   - stats every listed dependency header.

For a project with thousands of header dependencies after the ESP32 SDK
expansion, step 4 alone performs tens of thousands of `std::fs::metadata`
calls. On Windows, each metadata call hits the NTFS object manager and can
take tens of microseconds — tens of thousands × tens of µs ≈ tens of
seconds. This matches the observed 67.7 s.

## Top 3 Stalls

### 1. ESP32 warm-pass after daemon restart (~68 s → target: ~0.1 s)

**Phase**: the gap between `fast-path-check` returning "miss" and the
orchestrator emitting its first compile line, inside
`esp32/orchestrator.rs::build`.

**Why slow**: first build after a daemon restart hits the on-disk
fingerprint either (a) absent (because artifacts exist but no persisted
fingerprint), or (b) present but conservatively invalidated by the
`fp-watches` / zccache cross-check. Falling through invokes the full
pre-compile pipeline — library resolution, include assembly, framework lib
scan, per-source depfile walks — even though *every* compile call ends up
as a no-op because the object files are current.

**Expected saving**: ~67 s → ~0.1 s per first build (i.e., same as steady
state). On a FastLED repo with N sketches, this saves ~67 s × N on every
clean CI run.

**Suggested fix direction** (do NOT implement here):
- Persist the library-resolution result + include-path set + framework-lib
  inventory into the same fingerprint blob as the link-level result, so
  the fast-path can short-circuit without ever re-resolving libs.
- Alternatively: make the fast-path fingerprint *trusted* when the on-disk
  file set hash matches, without requiring the zccache layer to also agree.
  (Currently, when zccache is present, its verdict wins; when it's absent
  or can't be consulted, we fall back to `hash_watch_set_stamps`.)
- As a cheap partial fix: cache `fingerprint_watches` + their stamps in
  an in-memory `DashMap<project_dir, (hash, expiry)>` in the daemon context,
  so the second request never re-walks the file tree.

### 2. Second warm build still pays ~1.3 s even after the fingerprint hits

**Phase**: observed in run 2 of the ESP32 sequence — `fast-path-check=51 ms`
inside `esp32-orchestrator total=1321 ms`.

**Why slow**: even with the fast-path fingerprint matching, `compile_db_is_current`
re-reads + re-parses `compile_commands.json` from disk, and the subsequent
file-set hash over `fingerprint_watches` walks the project tree again. With
the daemon now caching results in memory, this work doesn't need to happen
twice in a row.

**Expected saving**: ~1.2 s on run 2, bringing it down to steady state (~80 ms).

**Suggested fix direction**: add an in-memory cache in
`DaemonContext` keyed by `(project_dir, env_name, profile)` → `{fingerprint_hash,
compile_db_mtime, last_checked}`. Invalidate on an upper-bound timestamp
(e.g. the most recent mtime seen in `fingerprint_watches` at the last
walk).

### 3. CLI daemon handshake on the very first call

**Phase**: `daemon-handshake` in `cli-build`. On a cold daemon this takes
~2.1 s; on a warm daemon it's ~1 ms.

**Why slow**: `ensure_daemon_running` polls with 100 ms intervals for up to
10 s after spawning. Even on a fast spawn (≤1 s to be ready), we reliably
sleep ≥1 × 100 ms + accumulated reqwest connect/retry delays = ~2.1 s.

**Expected saving**: ~2 s on the very first CLI invocation after a cold
machine (one-time, not per-sketch). Lower priority than #1 and #2 but easy
to fix.

**Suggested fix direction**: shorten the inner poll to 25 ms; bail on the
first successful `/api/health` response. Already tracked by the existing
retry budget; just tighten the initial interval.

## Ruled-out hypotheses

Measured and observed NOT to be hot on the minimal test projects:

- **`config-parse` / `board-load` / `build-dirs` / `flag-collect`**
  — all ≤8 ms. A real FastLED `platformio.ini` with dozens of env sections
  and `build_flags` could push this, but nowhere near 30 s.
- **`toolchain-ensure` / `framework-ensure`** — 6–15 ms. The cache lease
  + existence check is cheap because the daemon's `Package` machinery
  short-circuits on an already-installed package.
- **`lock-wait` (project mutex)** — 0 ms across 10+ measured runs. The
  in-memory per-project tokio Mutex is uncontended.
- **`source-scan`** — 2–5 ms on uno, untested on a real FastLED tree but
  unlikely to be the 30 s culprit given the speed on uno + the observed
  67 s delta on a near-empty ESP32 sketch.
- **Disk cache SQLite `reconcile`** — runs once at daemon startup in a
  background tokio task; does not block any build request. Verified by
  reading `crates/fbuild-daemon/src/main.rs:238–258`.
- **CLI subprocess / bash `time` oddities** — initially looked like warm
  builds took 2 minutes. Root cause: `tee`/`time` pipeline interaction on
  Windows MSYS bash. Replacing with Python `time.time()` deltas gave
  consistent, sensible numbers.

## Follow-up issues to file (suggested titles)

1. **"ESP32 orchestrator: memoize library + include resolution across
   calls within the same daemon"** — addresses the 67.7 s first-warm stall.
   Scope: add an in-memory cache keyed by `(project_dir, env_name,
   profile, metadata_hash)` that stores the fully-resolved `include_dirs`
   + `library_archives` + framework lib inventory, so the second-and-later
   calls skip `ensure_libraries_sync` and the `fw_libs` scan entirely.
2. **"ESP32 fast-path: avoid re-reading `compile_commands.json` after a
   fingerprint hit"** — addresses run-2's 1.3 s stall. Scope: cache
   `compile_db_is_current` result in `DaemonContext` with mtime-based
   invalidation.
3. **"CLI `ensure_daemon_running`: tighten initial poll interval from
   100 ms to 25 ms"** — addresses the 2 s first-call handshake.
4. **"Extend `FBUILD_PERF_LOG` instrumentation to cover the ESP32 deep
   path"** — add sub-phases for `library-manager`, `include-assembly`,
   `fw-lib-scan`, and `needs-rebuild-walk` inside the ESP32 orchestrator,
   so run-1 can be precisely attributed. Today the 67.7 s is a single
   opaque bucket.
5. **"Unrelated: daemon binds to wrong port on spawn from certain shells"**
   — seen in `~/.fbuild/dev/daemon/daemon.log` during this investigation
   (`failed to bind to 0.0.0.0:8765` while in dev mode). The second-spawned
   daemon crashed mid-download of the ESP32 toolchain. Not caused by
   #91 but worth its own ticket since it silently corrupts the download
   path.

## How to reproduce locally

```bash
# Windows, inside this worktree:
export FBUILD_DEV_MODE=1 FBUILD_PERF_LOG=1
export PATH="target/x86_64-pc-windows-msvc/release:$PATH"

# Build once (release) so the binaries match the instrumentation
soldr cargo build --release -p fbuild-cli -p fbuild-daemon

# Kill any stale daemon, then run a cold build to populate disk cache
fbuild.exe daemon stop
fbuild.exe build "$(pwd)/tests/platform/esp32dev" -e esp32dev   # ~14 min cold

# Stop the daemon so the next build is a "cold-daemon / warm-disk" pass
fbuild.exe daemon stop
# Check for stray processes:
tasklist | grep fbuild
# taskkill //F //PID <pid> as needed

# Now run the experiment
for i in 1 2 3 4 5; do
    time fbuild.exe build "$(pwd)/tests/platform/esp32dev" -e esp32dev
done
tail -30 ~/.fbuild/dev/daemon/daemon.log   # daemon-side per-phase summaries
```

Expected: run 1 = ~60–80 s wall, run 2 = ~1–2 s, runs 3+ = ~250–500 ms.

---

See also: [INDEX.md](INDEX.md) · [architecture/overview.md](architecture/overview.md) · [architecture/data-flow.md](architecture/data-flow.md)
