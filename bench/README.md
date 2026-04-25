# bench

End-to-end performance benchmarks that drive the `fbuild` CLI against real
sketches and frameworks, as opposed to per-crate micro-benchmarks.

Per-crate criterion benches live alongside their crate, e.g.:

- `crates/fbuild-header-scan/benches/scan_throughput.rs`
- `crates/fbuild-library-select/benches/resolve_cold.rs`

Run those with:

```bash
uv run soldr cargo bench -p fbuild-library-select --bench resolve_cold
uv run soldr cargo bench -p fbuild-header-scan  --bench scan_throughput
```

## Subdirectories

- [`fastled-examples/`](fastled-examples/README.md) — placeholder for the
  warm-cache library-selection harness across the FastLED examples matrix
  (`FastLED/fbuild#205` AC#5, P-01). Awaits Phase 4 zccache K/V memoization
  before there's a warm path to measure.

Other end-to-end matrices (whole-build wall-clock, deploy+flash latency,
emulator boot) may join this directory in the future. Each subdirectory
must carry its own `README.md` explaining what it measures, how to run it,
and which CI gate (if any) it feeds.
