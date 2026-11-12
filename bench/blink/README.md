# Blink build comparison

This directory is one shared Arduino Uno and ESP32-S3 Blink sketch for the nightly
whole-build comparison. Arduino CLI, PlatformIO, and fbuild compile the exact
same `blink.ino` file for both targets. Each ecosystem's framework distribution
is pinned independently in the benchmark workflow and `platformio.ini`.

Each tool is measured for three trials by default:

- **Cold** is a first-time install. Every tool's project outputs, caches,
  packages and toolchains are removed before the timer starts, on both boards,
  so all downloads are inside the timed build.
- **Warm** immediately repeats the same build without changing any input.

Only cleanup is untimed. Arduino CLI loses its `packages/` and `staging/`
directories and its timed window includes `arduino-cli core install`, because
`arduino-cli compile` will not install a core. PlatformIO loses its entire
home directory (pioarduino also reuses `dist` and `tools`) and `.pio`. fbuild runs `fbuild clean cache`,
stops its daemon, and empties its cache root and project `.fbuild`.

ESP32-S3 uses Arduino-ESP32 3.3.12 in all three tools (pioarduino `55.03.312`,
which needs PlatformIO Core 6.2.0) because fbuild publishes per-chip library
slices for that release. The older `espressif32@6.x` stack ships one combined
bundle, which has nothing to slice.

Each cold trial records the bytes received (`cold_download_bytes`) from
`/proc/net/dev`. On a shaped link, a cold build that received under 1 MB fails
the run, since its packages must have survived cleanup.

The Rust runner records every trial and publishes the median. Cold bars are
drawn behind narrower warm overlays using the same GitHub-dark gray, blue, and
red palette as the zccache and soldr benchmark graphics.

## Cold-build breakdown

FastLED/fbuild#1465. Besides the three tool timings, each run records where
fbuild's cold build spends its time:

- **Raw-compiler floor (`raw_baseline_ms`)** — fbuild's
  `compile_commands.raw.json` (the real `avr-gcc`/`avr-g++` invocations,
  FastLED/fbuild#1467) replayed directly, in parallel (one job per `nproc` core), with no fbuild,
  daemon, or zccache in the loop. This is the fastest the same compile set can
  run on the runner. It covers compilation only; the link is not replayed.
- **Per-phase fbuild timings (`cold_phases_ms`)** — the harness sets
  `FBUILD_PERF_LOG_JSON` for the fbuild cold trials, so fbuild writes a
  JSON perf log with one entry per phase the pipeline reports (e.g.
  `compile-core`, `compile-variant`, `compile-sketch`, `compile-db`, `link`
  (link + objcopy + size), `build-info`). The published value is the per-phase median across
  cold trials; raw per-trial maps are kept in `cold_phase_trials`.
- **`fbuild_overhead_ms`** — fbuild cold median minus `raw_baseline_ms`: time
  fbuild spends above the raw compiler floor. `null` when the floor could not
  be measured.
- **`fbuild_vs_platformio_cold`** — fbuild cold median divided by the
  PlatformIO cold median (below 1.0 means fbuild is faster).

zccache miss overhead is not yet measured directly; until zccache exposes
per-TU child time, approximate it as `compile-core` minus the raw floor's
share for the same translation units.

**Regression warning.** After writing `history.jsonl`, the harness compares
this run's `fbuild_vs_platformio_cold` against the median of the entries
from the last 7 days and emits a GitHub `::warning` annotation when it is
higher. The workflow restores the rolling history before the harness runs so
the median is available, and writes these fields to the job step summary.

## Run locally

Install Arduino CLI and PlatformIO Core first. The ESP32-S3 environment pulls the
pioarduino `55.03.312` bundle, which needs **PlatformIO Core 6.2.0**; a
distribution `pio` (6.1.x) cannot build that platform. Arduino CLI 1.1.x is
enough.

> **Warning: every cold trial deletes installs, untimed, before the timer
> starts.** For each board and tool the preparation step removes
> `<arduino data>/packages` and `staging`, the whole `PLATFORMIO_HOME`
> (pioarduino also reuses `dist` and `tools`), the whole fbuild cache root, and
> the project's `.pio` and `.fbuild`. With default locations that means your
> real `~/.platformio` and fbuild cache. Point each tool at a scratch directory
> instead; the harness cleans only what these variables name. It reads
> `PLATFORMIO_HOME`, while PlatformIO itself reads `PLATFORMIO_CORE_DIR`, so
> export both:

```bash
S=/path/to/scratch/bench-blink
export PLATFORMIO_HOME=$S/platformio PLATFORMIO_CORE_DIR=$S/platformio
export ARDUINO_DIRECTORIES_DATA=$S/arduino \
       ARDUINO_DIRECTORIES_DOWNLOADS=$S/arduino/staging \
       ARDUINO_DIRECTORIES_USER=$S/arduino/user
export FBUILD_CACHE_DIR=$S/fb-cache
```

Then build fbuild and run the harness through `soldr`:

```bash
soldr cargo build --release -p fbuild-cli --bin fbuild -p fbuild-daemon --bin fbuild-daemon
soldr cargo run --release -p fbuild-bench-fastled-examples \
  --bin bench-build-comparison -- \
  --project-dir bench/blink \
  --fbuild target/release/fbuild \
  --arduino-cli $S/bin/arduino-cli \
  --platformio $S/pio-venv/bin/pio \
  --output-dir $S/out \
  --trials 1
```

`--arduino-cli` and `--platformio` default to a PATH lookup, `--trials`
defaults to 3, and `--network-mbit` shapes the cold download window the way CI
does (0, the default, is unshaped). The artifacts land in `--output-dir` in the
same layout the nightly publishes.

`--boards` narrows the matrix to the board keys you name (`uno`, `esp32s3`),
which is how a developer iterates: the whole matrix costs ~25 minutes and ~14 GB
of scratch because of the ESP32-S3 toolchains, while `--boards uno` finishes in
about a minute once the toolchains are installed. An unknown key fails the run
and lists the valid ones, so a typo cannot silently benchmark nothing.

Cold preparation is destructive by design, so the harness refuses to start when
`PLATFORMIO_HOME`, `FBUILD_CACHE_DIR` or `ARDUINO_DIRECTORIES_DATA` is unset —
those defaults are the real `~/.platformio`, the warm fbuild cache and
`~/.arduino15`. The nightly runner passes `--allow-default-homes` because it is
throwaway and is meant to start cold.

The `target/release/fbuild` path above is for Linux and CI. On Windows, use
`target/x86_64-pc-windows-msvc/release/fbuild.exe`.

Budget: with `--trials 1` on a 16-core Linux host on an unshaped link the whole
matrix takes about 25 minutes and about 14 GB of scratch (Arduino CLI 7.8 GB,
PlatformIO 5.9 GB, fbuild cache 0.3 GB), almost all of it ESP32-S3 toolchains.
A single board is about a minute; `--boards uno --trials 1` measured 48.9 s
(Arduino CLI 15.52 s cold / 550 ms warm, PlatformIO 18.63 s / 630 ms, fbuild
11.45 s / 73 ms) on 2026-11-11 against `a9e572ba`, unshaped.

### Recorded local run

2026-11-11 against origin/main `9933d9e9`, 16-core Linux, unshaped link,
`--trials 1`. Absolute numbers are not comparable with the nightly (shaped link,
median of 3, cold runner); the fbuild/PlatformIO ratio is.

| Board | Tool | Cold | Warm | Cold download |
|---|---|---|---|---|
| Arduino Uno | Arduino CLI | 20.56 s | 593 ms | 70 MB |
| Arduino Uno | PlatformIO | 20.24 s | 668 ms | 62 MB |
| Arduino Uno | fbuild | 19.16 s | 65 ms | 68 MB |
| ESP32-S3 | Arduino CLI | 490.7 s | 5.91 s | 1.16 GB |
| ESP32-S3 | PlatformIO | 257.3 s | 4.45 s | 691 MB |
| ESP32-S3 | fbuild | 60.8 s | 53 ms | 242 MB |

`fbuild_vs_platformio_cold` is 0.946 (Uno) and 0.236 (ESP32-S3), in the same
direction as the published nightly (0.88 and 0.31). fbuild's ESP32-S3 cold
window is dominated by `pioarduino-resolve` (52.4 s of 60.8 s in
`fbuild-perf.jsonl`), the one-time framework download; the raw compile floor was
2.6 s and 121 ms for Uno.

The nightly workflow installs and records pinned Arduino CLI and PlatformIO
versions, primes only the Arduino package index, carries forward a
365-run `history.jsonl`, and publishes:

- `manifest.json` — stable discovery index for agents and other clients
- `latest.json` — full metadata, raw trials, and medians for the newest run
- `history.jsonl` — rolling compact history
- `benchmark.svg` — README graphic with cold/warm overlays
- `index.html` — human-facing GitHub Pages report

The `benchmark-stats` branch is force-published from a fresh repository on
every successful default-branch run, so its Git history always contains one
generated commit.
