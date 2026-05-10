# bench/fastled-examples

Warm-cache library-selection benchmark across a curated FastLED examples
matrix. This is the AC#5 / P-01 measurement for
[`FastLED/fbuild#205`](https://github.com/FastLED/fbuild/issues/205).

## What it measures

For each example sketch under `$FASTLED_DIR/examples/`, runs the
`fbuild_library_select::cache::resolve_cached` resolver twice against a
fresh `KvStore`:

- **Cold** — empty cache. Wall-clock includes the scanner walk over the
  FastLED `src/` tree (~1000 files), the 2-pass LDF reconciliation, and
  the cache write. This dominates total time.
- **Warm** — cache pre-populated. Wall-clock includes the cache-key
  compute (sorted seed/header content hashing, bounded by `cache_key`
  itself) and the bincode decode of the cached `Selection`.
  `from_cache = true` is asserted so silent re-misses surface
  immediately.

The framework library set is a synthetic Teensyduino-class stub built
via `MiniFramework`. The bench measures resolver throughput, not the
correctness of which libraries get selected — that is the acceptance-test
layer (`crates/fbuild-build/tests/teensylc_acceptance.rs`).

## Running

`FASTLED_DIR` is required — there is no implicit default, since the
correct path is host-dependent (CI uses `external/fastled` from the
workflow checkout, developers use whatever convention they like) and a
silent fallback would mask configuration mistakes.

```bash
FASTLED_DIR=/path/to/fastled \
  uv run soldr cargo run --release -p fbuild-bench-fastled-examples

# Emit a JSON report alongside stdout
FASTLED_DIR=/path/to/fastled \
  uv run soldr cargo run --release -p fbuild-bench-fastled-examples \
  -- --json bench/fastled-examples/report.json
```

If any example fails to measure (missing sketch, KvStore error, warm
miss) the binary exits non-zero rather than skipping the row. CI must
treat a partial matrix as a failure, not a pass.

## Sample numbers

Captured 2026-05-10 on Windows / Ryzen workstation, FastLED `main`,
release build:

| example       | cold (ms) | warm (ms) | speedup |
|---------------|----------:|----------:|--------:|
| Blink         |    923.58 |     11.36 |   81.3x |
| Pacifica      |    915.98 |     12.64 |   72.4x |
| Animartrix    |    970.14 |     11.76 |   82.5x |
| Audio         |    830.51 |     11.74 |   70.7x |
| BlurBenchmark |    827.46 |     10.48 |   79.0x |
| Chromancer    |    844.13 |     10.89 |   77.5x |

The warm path comfortably clears AC#5 (≤ +50 ms over current fbuild) at
~11 ms per example. The ~75x speedup reflects the cost asymmetry between
walking the FastLED `src/` tree (~1000 files) and a `KvStore`
get + bincode decode of a serialized `Selection`.

## Curated example set

The harness intentionally runs a small representative subset rather than
all 80+ examples. Adding more is cheap — see `EXAMPLES` in `src/main.rs`.
The current set spans:

- Trivial single-strip sketches (`Blink`)
- Animation-heavy sketches (`Pacifica`, `Animartrix`)
- I/O-heavy sketches (`Audio`)
- Throughput stress sketches (`BlurBenchmark`, `Chromancer`)

## CI

The `fastled-examples` job in `.github/workflows/bench-205.yml` is
`workflow_dispatch`-only because it requires a FastLED checkout. CI
checks out FastLED at a pinned release tag (currently `3.10.3`) so
measurements are reproducible, then runs the bench and uploads the JSON
report as an artifact. Bumping the pin is a deliberate baseline event —
update both the workflow `ref:` and the sample-numbers table above in
lockstep.

There is no automatic CI gate on the warm timings yet — first capture a
stable cross-runner baseline, then a follow-up adds the threshold gate.

## Cross-links

- Issue: [`FastLED/fbuild#205`](https://github.com/FastLED/fbuild/issues/205)
- This harness: [`FastLED/fbuild#218`](https://github.com/FastLED/fbuild/issues/218)
- Per-crate synthetic warm bench:
  [`crates/fbuild-library-select/benches/resolve_warm.rs`](../../crates/fbuild-library-select/benches/resolve_warm.rs)
- Subsystem architecture:
  [`docs/architecture/library-selection.md`](../../docs/architecture/library-selection.md)
