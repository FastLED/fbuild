# Blink build comparison

This directory is one shared Arduino Uno Blink sketch for the nightly
whole-build comparison. Arduino CLI, PlatformIO, and fbuild compile the exact
same `blink.ino` file for the Arduino Uno target. Each ecosystem's framework
distribution is pinned independently: `arduino:avr@1.8.8` for Arduino CLI and
`atmelavr@5.1.0` for PlatformIO/fbuild.

Each tool is measured for three trials by default:

- **Cold** removes every tool's project outputs, reusable framework objects,
  and compiler-object caches before compiling. Arduino CLI and PlatformIO
  download/HTTP caches are also cleared. Installed packages/toolchains and
  fbuild package archives remain available.
- **Warm** immediately repeats the same build without changing any input.

Cleanup runs before the timer for every cold trial. Arduino CLI runs
`arduino-cli cache clean` and removes its build directory; PlatformIO runs
`pio system prune --cache --force` and its project clean target; fbuild runs
`fbuild clean cache`. None of these operations uninstalls the pinned packages.

The Rust runner records every trial and publishes the median. Cold bars are
drawn behind narrower warm overlays using the same GitHub-dark gray, blue, and
red palette as the zccache and soldr benchmark graphics.

## Run locally

Install Arduino CLI, its `arduino:avr` core, and PlatformIO first. Build fbuild
and run the harness through `soldr`:

```bash
soldr cargo build --release -p fbuild-cli --bin fbuild -p fbuild-daemon --bin fbuild-daemon
soldr cargo run --release -p fbuild-bench-fastled-examples \
  --bin bench-build-comparison -- \
  --project-dir bench/blink \
  --fbuild target/release/fbuild \
  --output-dir benchmark-stats
```

The `target/release/fbuild` path above is for Linux and CI. On Windows, use
`target/x86_64-pc-windows-msvc/release/fbuild.exe`.

The nightly workflow installs and records pinned Arduino CLI and PlatformIO
versions, primes package downloads outside the timed region, carries forward a
365-run `history.jsonl`, and publishes:

- `manifest.json` — stable discovery index for agents and other clients
- `latest.json` — full metadata, raw trials, and medians for the newest run
- `history.jsonl` — rolling compact history
- `benchmark.svg` — README graphic with cold/warm overlays
- `index.html` — human-facing GitHub Pages report

The `benchmark-stats` branch is force-published from a fresh repository on
every successful default-branch run, so its Git history always contains one
generated commit.
