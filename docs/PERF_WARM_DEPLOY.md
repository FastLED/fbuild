# Warm-Deploy Loop Results (FastLED/fbuild#114)

First-pass measurement of the warm **rebuild + deploy + monitor reconnect**
path against real ESP32-S3 hardware, against the 4 000 ms end-to-end budget
defined in #114.

## TL;DR

- **5/5 consecutive iterations** of the full `deploy + monitor reattach +
  first-byte-from-device` loop land at **3.585–3.697 s** — all inside the
  4 000 ms budget with ~300–400 ms of slack.
- Deploy-only warm path (`fbuild deploy` without `--monitor`) is **~2.6 s**
  steady-state (~3.3 s on the first call after daemon spawn).
- Acceptance criterion from #114 (*three consecutive in-budget iterations*)
  is met; loop spec (`LOOP.md`) is retired.

## Methodology

- Host: Windows 10 Pro, x86_64-pc-windows-msvc.
- Binary: `target/x86_64-pc-windows-msvc/release/fbuild.exe` built from
  `feat/114-warm-deploy-loop-first-pass`.
- Project: `tests/platform/esp32s3/` (Arduino, `ARDUINO_USB_CDC_ON_BOOT=1`,
  blinking LED + `Serial.println("Hello from ESP32-S3!")` once per 1 s loop).
- Device: ESP32-S3-DevKitC-1, native USB-CDC on COM13 (`303a:1001`).
- Daemon: in-process via CLI auto-spawn, persistent across iterations.
- Pre-condition: cold deploy (full flash, 3m 16s) brings the device to the
  exact image the warm path then verify-skips against.

## Results

### Deploy-only warm path (no monitor)

`fbuild deploy -e esp32s3 -p COM13`

| iter | wall-clock | server-side outcome |
|---:|---:|---|
| 1 | 3.257 s | `verify skipped, device already matched` (incl. daemon warm-up) |
| 2 | 2.642 s | `verify skipped, device already matched` |
| 3 | 2.640 s | `verify skipped, device already matched` |
| 4 | 2.615 s | `verify skipped, device already matched` |
| 5 | 2.568 s | `verify skipped, device already matched` |

### Full loop (T1 + T2 + T3 + TTFB)

`fbuild deploy -e esp32s3 -p COM13 --monitor --halt-on-success "Hello from ESP32-S3" --timeout 5`

| iter | wall-clock | budget | margin |
|---:|---:|---:|---:|
| 1 | 3.587 s | 4.000 s | +413 ms |
| 2 | 3.590 s | 4.000 s | +410 ms |
| 3 | 3.697 s | 4.000 s | +303 ms |
| 4 | 3.585 s | 4.000 s | +415 ms |
| 5 | 3.605 s | 4.000 s | +395 ms |

Mean 3.613 s, max 3.697 s. The ~1 s overhead between the deploy-only path
and the full loop is dominated by the test sketch's `delay(500)` × 2 loop
period — TTFB is the next `Serial.println` cadence, not a property of the
deploy/reconnect path. A sketch emitting at boot would shave that ~1 s.

## What got us here

Landed before this measurement:

- **#116** — `FBUILD_TRUST_DEVICE_HASH=1` opt-in trust-skip on verify-flash
  (server cost: ~1.5 s → ~50 ms).
- **#118** — `ImageHashMemo` (skip SHA-256 of unchanged firmware) +
  `DeviceManager::refresh_devices_if_stale(2s)` (~50 ms → ~1–2 ms server-side).
- **#120** — `DaemonWatchSetCache` removes the `fp-watches-collect` walk on
  back-to-back warm builds.

The full loop runs cleanly inside the 4 s envelope without any phase
breaching its individual budget; no follow-up perf issues are filed against
#114.

## Closing the loop

Acceptance criteria from #114:

- [x] LOOP.md committed (#113), and later untracked (#115); local-only
      scratch, removed.
- [x] First pass of the loop against real ESP32-S3 hardware to confirm or
      tune budgets — this document.
- [x] Follow-up issues for any phase that consistently breaches its
      budget — none; all five iterations green.
- [x] T1–T3 land inside the total budget for three consecutive iterations —
      five consecutive, lowest margin 303 ms.
