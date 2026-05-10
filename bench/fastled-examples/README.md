# bench/fastled-examples

Warm-cache library-selection benchmarks across the FastLED examples matrix.
This is the harness referenced by `FastLED/fbuild#205` for acceptance
criterion **AC#5 / P-01**:

> Warm library-selection on FastLED examples matrix `<= current fbuild
> + 50 ms`.

## Status: empty placeholder

There is no harness in this directory yet — the real per-board, per-example
matrix needs a checked-out FastLED tree (`~/dev/fastled`) and orchestrator
wiring that routes through `resolve_cached`. That work is tracked
separately. The synthetic warm baseline (`MiniFramework`-backed cache hit,
no real FastLED) already exists at
[`../../crates/fbuild-library-select/benches/resolve_warm.rs`](../../crates/fbuild-library-select/benches/resolve_warm.rs)
and is the first-pass regression guard for the cache-hit path.

Phase 4 K/V memoization itself shipped in PR #212, so the warm path is
real and measurable today; what is missing here is the multi-board,
real-sketch matrix that AC#5 requires.

## The plan once the FastLED tree is wired in

1. Iterate the FastLED examples tree (`~/dev/fastled/examples/**`) under
   each supported board: at minimum `teensyLC`, `teensy30`, `teensy41`,
   `stm32f103c8`, `esp32-s3`, `uno`, `ws2812`. The matrix expands with
   board coverage.
2. For each `(example, board)` pair, run the resolver twice:
   - **Cold pass.** Empty `~/.zccache/`. Captures the K/V miss path and
     the underlying scan + walk + LDF cost. This is the P-02 lane.
   - **Warm pass.** Populated `~/.zccache/`. Captures the K/V hit path,
     where the only work should be cache lookup + result deserialization.
     This is the P-01 lane.
3. Diff the warm scan time against a captured baseline checked into
   `tasks/baseline-205.md`. CI fails the job if any `(example, board)`
   regresses the warm path by more than 50 ms vs. that baseline (the
   `#205` AC#5 threshold).
4. Emit a structured JSON report (`bench/fastled-examples/report.json`)
   that future PR comments can diff. Format TBD with the harness.

## Running the synthetic mini benches today

The closest signal available right now without a FastLED checkout is the
per-crate cold and warm criterion benches against `MiniFramework`:

```bash
uv run soldr cargo bench -p fbuild-library-select --bench resolve_cold
uv run soldr cargo bench -p fbuild-library-select --bench resolve_warm
```

Those benches drive a synthetic ~30-library Teensyduino-class tree built
from `fbuild-test-support`'s `MiniFramework` rather than real FastLED
sketches. They are useful regression guards for the resolver and its
cache layer respectively, but they do **not** satisfy AC#5 on their own.

## Cross-links

- Issue: `FastLED/fbuild#205`
- Phase 4 K/V memoization (shipped in #212):
  [`../../tasks/zccache-kv-design.md`](../../tasks/zccache-kv-design.md)
- Subsystem architecture:
  [`../../docs/architecture/library-selection.md`](../../docs/architecture/library-selection.md)
- Foundation baseline that the warm threshold compares against:
  [`../../tasks/baseline-205.md`](../../tasks/baseline-205.md)
- Per-crate cold + warm benches (different scope, same subsystem):
  [`../../crates/fbuild-library-select/benches/README.md`](../../crates/fbuild-library-select/benches/README.md),
  [`../../crates/fbuild-header-scan/benches/README.md`](../../crates/fbuild-header-scan/benches/README.md)
