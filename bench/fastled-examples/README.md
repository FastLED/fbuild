# bench/fastled-examples

Warm-cache library-selection benchmarks across the FastLED examples matrix.
This is the harness referenced by `FastLED/fbuild#205` for acceptance
criterion **AC#5 / P-01**:

> Warm library-selection on FastLED examples matrix `<= current fbuild
> + 50 ms`.

## Status: empty placeholder

There is no harness here yet, and on purpose. P-01 measures the **warm**
path through the resolver, which depends on the zccache K/V memoization
delivered by `#205` Phase 4 (gated on `zackees/zccache#130`). Until that
lands, every `resolve()` call is cold and there is no warm number to
gate against. Adding a "warm-ish" harness today would just measure cold
work twice and produce a misleading baseline.

When Phase 4 ships and a `zccache` release is cut, a follow-up PR drops
the actual harness into this directory and wires it into CI.

## The plan once Phase 4 lands

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

## Running a partial version today

The closest signal available right now is the per-crate cold-resolve
criterion bench:

```bash
uv run soldr cargo bench -p fbuild-library-select --bench resolve_cold
```

That bench drives a synthetic ~30-library Teensyduino-class tree built
from `fbuild-test-support`'s `MiniFramework` rather than real FastLED
sketches, and it measures the cold path only. It is a useful regression
guard for the resolver itself, but it does **not** satisfy AC#5.

## Cross-links

- Issue: `FastLED/fbuild#205`
- Phase 4 design note (the prerequisite for this directory):
  [`../../tasks/zccache-kv-design.md`](../../tasks/zccache-kv-design.md)
- Subsystem architecture:
  [`../../docs/architecture/library-selection.md`](../../docs/architecture/library-selection.md)
- Foundation baseline that the warm threshold compares against:
  [`../../tasks/baseline-205.md`](../../tasks/baseline-205.md)
- Per-crate cold benches (different scope, same subsystem):
  [`../../crates/fbuild-library-select/benches/README.md`](../../crates/fbuild-library-select/benches/README.md),
  [`../../crates/fbuild-header-scan/benches/README.md`](../../crates/fbuild-header-scan/benches/README.md)
