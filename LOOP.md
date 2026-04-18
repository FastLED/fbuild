# Warm-Deploy Agent Loop

An agent-driven performance loop for the warm **rebuild + deploy + monitor
reconnect** path. The acceptance target is **< 4 s end-to-end** when:

1. No source, config, or toolchain changes since the last build.
2. The device already holds the exact firmware image (verify-flash MD5 match).
3. The serial monitor is expected to reattach and receive the first JSON-RPC
   byte from the device.

"Auto-research" in the original request refers to the
[`auto_reconnect`](crates/fbuild-python/src/lib.rs) behaviour of the Python
`SerialMonitor` — the monitor that re-establishes its WebSocket + port after a
deploy preempts the serial session.

## Budget tree (total: 4 000 ms)

| Phase | Budget | Source of truth |
|---|---|---|
| CLI → daemon handshake | 200 ms | `cli-build` / `cli-deploy` perf-log |
| Daemon lock + dispatch | 100 ms | `daemon-handler` perf-log (`lock-wait`, `total`) |
| Warm-build fingerprint hit (no compile, no link) | 500 ms | `esp32-orchestrator` perf-log (`fast-path-check`, `compile-*=0 ms`) |
| Verify-flash MD5 match (3 regions) | 1 500 ms | daemon `tracing::info!("verify-flash: device already running this exact image; skipping write")` |
| Serial port re-open + WebSocket reattach | 700 ms | `SharedSerialManager` retry schedule + `/ws/serial-monitor` handshake |
| TTFB (first JSON-RPC byte from device) | 1 000 ms | monitor WebSocket `monitor_data` frame |
| Slack | 0 ms | (budget is tight — any regression should trip the loop) |

The individual budgets can be traded off (e.g. a fast verify saves room for a
slow reattach) as long as the end-to-end total stays under 4 000 ms.

## Prerequisites

- ESP32-S3 DevKit attached on a known port (`COM*` on Windows,
  `/dev/ttyUSB*` or `/dev/ttyACM*` elsewhere).
- Device is already flashed with the image under test.
- Daemon running in dev mode: `FBUILD_DEV_MODE=1 uv run cargo run -p fbuild-cli -- daemon start`.
- Env toggles for the native flasher path (issue #66):
  `FBUILD_USE_ESPFLASH_VERIFY=1 FBUILD_USE_ESPFLASH_WRITE=1`.
- Perf logging on: `FBUILD_PERF_LOG=1` in the daemon's env (dominates the
  numbers; see [`docs/PERF_WARM_BUILD.md`](docs/PERF_WARM_BUILD.md)).

## Test matrix

Each row is one iteration the loop captures. All runs happen against the same
project + environment pair (`tests/platform/esp32s3` / `-e esp32s3`) so results
are directly comparable.

| # | Test | Setup before run | Expected outcome |
|---|---|---|---|
| T0 | **Cold baseline** | `rm -rf .fbuild`; fresh daemon | Establish worst case (single full build + full write-flash). Not part of the 4 s budget — used only as a comparison point. |
| T1 | **Warm no-change rebuild** | Immediately after T0, no edits | Fingerprint hit, orchestrator reports `compile-*=0 ms`, `link` skipped, total build ≤ 500 ms. |
| T2 | **Warm + verify-skip deploy** | T1 followed by `deploy` (no `--monitor`) | Daemon emits `verify-flash: ... skipping write`, `outcome=VerifySkip`, deploy total ≤ 2 000 ms. |
| T3 | **Warm + deploy + monitor reconnect** | T2 with `--monitor` | Full loop end-to-end ≤ 4 000 ms, first `monitor_data` WebSocket frame observed. |
| T4 | **Comment-only edit** | Touch the `.ino` (whitespace change) | Fingerprint invalidates → short recompile only for the sketch TU → total ≤ 2 500 ms. |
| T5 | **Real source edit** | Add a `Serial.println("x")` line | Fingerprint invalidates → sketch recompile + relink + full write-flash → no budget (used to confirm the loop notices real changes). |

## One iteration

Run the loop against T1–T3 (T0 is a one-time prelude, T4/T5 are diagnostics).
Each iteration does this, in order:

1. **Stamp start**: record wall-clock start, daemon PID, git SHA.
2. **Run the command** under `FBUILD_PERF_LOG=1`, tee the stderr to
   `tasks/loop-runs/<ts>-<test>.log`:
   ```
   uv run cargo run -p fbuild-cli -- deploy tests/platform/esp32s3 \
       -e esp32s3 --port $PORT --monitor
   ```
3. **Parse perf-log lines** with the grammar
   `[perf-log <scope>] <phase>=<ms> ms, ..., total=<ms> ms`. Scopes of
   interest: `cli-deploy`, `daemon-handler`, `esp32-orchestrator`, `pipeline`.
4. **Parse verify-flash** outcome: grep for
   `verify-flash: .* skipping write` (match) vs
   `verify-flash: .* rewriting regions` (mismatch).
5. **Measure TTFB**: from the deploy-success timestamp, the loop watches
   `/ws/serial-monitor` (or the CLI's `--monitor` output) for the first line
   of device output and records the delta.
6. **Compute end-to-end**: CLI wall-clock from step 2.
7. **Append** a CSV row to `tasks/loop-runs/RESULTS.csv`:
   ```
   timestamp,test,total_ms,build_ms,verify_outcome,deploy_ms,reconnect_ms,ttfb_ms,sha,notes
   ```

## Decision rules (what the agent does after each run)

After each capture the agent evaluates in this order and takes **one** action:

1. **All budgets met, T1–T3 green for 3 consecutive iterations** →
   write a summary to `tasks/loop-runs/SUMMARY.md`, stop the loop, and post a
   closing comment on the tracking issue.

2. **Warm build > 500 ms** (T1) →
   inspect the dominant `esp32-orchestrator` phase. Known suspects from
   [`docs/PERF_WARM_BUILD.md`](docs/PERF_WARM_BUILD.md):
   `fp-watches-collect`, `framework-ensure`, `source-scan`. File or update a
   sub-issue with the phase name + observed ms, and move on.

3. **Verify-flash skipped but deploy > 2 000 ms** (T2) →
   the 6 s → 1.5 s path is espflash-native. Confirm
   `FBUILD_USE_ESPFLASH_VERIFY=1` is set on the daemon. If already on, log the
   three per-region MD5 times (espflash emits `verify region …` lines at
   `debug`) and file a follow-up.

4. **Deploy fine but TTFB > 1 000 ms** (T3) →
   capture the `SharedSerialManager` retry sequence from daemon logs. On
   Windows, USB-CDC re-enumeration has a floor of ~700 ms and the retry
   schedule adds 250 ms × N. If the first retry already succeeded, TTFB is
   device-boot time, not reconnect — note that in `notes`.

5. **Deploy ran write-flash when it shouldn't have** (T2/T3) →
   the MD5 didn't match. Either the build produced a non-deterministic
   artefact (build time stamp inside firmware?) or the device was power-cycled
   between runs. Diff the firmware against the previous run's cached copy in
   `.fbuild/last-deploy/` and file a bug if bytes differ with no source
   change.

6. **T4 regressed into a full rebuild** →
   fingerprint invalidation is too coarse. Capture which phase the
   orchestrator re-ran (`compile-sketch=… ms` > 0 but other `compile-*=0`).

7. **Repeat 5 iterations without meeting (1)** → stop and file an
   issue summarising the stuck phase; do not loop forever.

## Cadence

- **Self-paced.** Pace each iteration to natural checkpoints, not a timer.
- Between iterations, if a code change was made, wait for
  `uv run cargo check --workspace --all-targets` and
  `uv run cargo clippy --workspace --all-targets -- -D warnings` to pass
  before re-running; a broken daemon will mis-report timings.
- If no change was made, sleep ~90 s to let any background indexing settle
  (Windows Defender, etc.) and re-run to confirm reproducibility.

## Outputs

Everything the loop produces lives under `tasks/loop-runs/`:

- `RESULTS.csv` — append-only row per iteration.
- `<ts>-<test>.log` — raw stderr from each CLI invocation.
- `SUMMARY.md` — written on termination (success or max-attempts).

## Ground-truth references

- [`docs/PERF_WARM_BUILD.md`](docs/PERF_WARM_BUILD.md) — existing warm-build
  investigation, phase names, and known stalls.
- [`crates/fbuild-build/src/perf_log.rs`](crates/fbuild-build/src/perf_log.rs)
  — `FBUILD_PERF_LOG=1` emitter; grammar for log lines.
- [`crates/fbuild-daemon/src/handlers/operations.rs`](crates/fbuild-daemon/src/handlers/operations.rs)
  — verify-skip call site; emits the "skipping write" log line.
- [`crates/fbuild-serial/src/preemption.rs`](crates/fbuild-serial/src/preemption.rs)
  and [`manager.rs`](crates/fbuild-serial/src/manager.rs) — reconnect retry
  schedule after deploy preemption.
- [`crates/fbuild-python/src/lib.rs`](crates/fbuild-python/src/lib.rs) —
  `SerialMonitor.auto_reconnect` (the "auto-research" behaviour).

## Invoking the loop

Two ways to drive this spec:

1. **/loop skill (recommended)** — let Claude Code self-pace:
   ```
   /loop @./LOOP.md
   ```
   The loop skill calls `ScheduleWakeup` between iterations based on the
   cadence rules above.

2. **Manual** — run the steps under "One iteration" by hand, append to the
   CSV, and judge against the budget tree yourself. Useful for a one-off
   check after a suspected regression.
