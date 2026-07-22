# bench

End-to-end performance benchmarks that drive the `fbuild` CLI against real
sketches and frameworks, as opposed to per-crate micro-benchmarks.

Per-crate criterion benches live alongside their crate, e.g.:

- `crates/fbuild-header-scan/benches/scan_throughput.rs`
- `crates/fbuild-library-select/benches/resolve_cold.rs`
- `crates/fbuild-library-select/benches/resolve_warm.rs`

Run those with:

```bash
soldr cargo bench -p fbuild-library-select --bench resolve_cold
soldr cargo bench -p fbuild-library-select --bench resolve_warm
soldr cargo bench -p fbuild-header-scan  --bench scan_throughput
```

## Subdirectories

- [`blink/`](blink/README.md) — shared Arduino Uno Blink fixture used by the
  nightly Arduino CLI vs PlatformIO vs fbuild whole-build benchmark.
- [`fastled-examples/`](fastled-examples/README.md) — real-FastLED
  warm-cache library-selection matrix (`FastLED/fbuild#205` AC#5, P-01).
  Discovers examples under `$FASTLED_DIR` (default `~/dev/fastled`),
  runs the resolver cold + warm per example, and reports timings.
  Run with `soldr cargo run --release -p fbuild-bench-fastled-examples`.
  The synthetic warm-path baseline lives in
  `crates/fbuild-library-select/benches/resolve_warm.rs`.

Other end-to-end matrices (whole-build wall-clock, deploy+flash latency,
emulator boot) may join this directory in the future. Each subdirectory
must carry its own `README.md` explaining what it measures, how to run it,
and which CI gate (if any) it feeds.

The nightly whole-build comparison is published at
<https://fastled.github.io/fbuild/>. Its machine-readable discovery document
is `manifest.json` on the one-commit `benchmark-stats` branch.
