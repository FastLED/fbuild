# fbuild-library-select benches

Criterion benchmarks for the PlatformIO-LDF-style library resolver.

## resolve_cold

End-to-end cold-path measurement of `resolve()` against a synthetic
~30-library framework tree (Teensyduino-class) built with `MiniFramework`. A
5-deep transitive include chain forces the two-pass LDF reconciliation; the
remaining libraries are unreferenced and must be rejected — that doubles as a
guard against the #204 over-selection regression. Walks the tempdir on every
iteration; the bench calls `resolve()` directly, so no cache layer ever sits
in front of it.

The Phase 7 P-02 threshold from FastLED/fbuild#205 is **≤ 200 ms cold for a
typical teensy41 project**. This bench captures the baseline; future PRs gate
against it.

Run:

```bash
soldr cargo bench -p fbuild-library-select --bench resolve_cold
```

## resolve_warm

Warm cache-hit path through `resolve_cached()` against the same synthetic
~30-library `MiniFramework` tree. The bench builds the fixture once, opens a
`FileKvStore` in a tempdir, primes the cache with one untimed `resolve_cached`
call (which misses), then times only the second invocation. Each iteration
asserts `from_cache == true` and panics otherwise — that way we can never
silently regress to measuring miss work.

This is the Phase 7 / #215 P-01-mini bench. The real per-board, per-example
warm matrix lives at `bench/fastled-examples/` and depends on a checked-out
FastLED tree; the synthetic mini bench here is the cache-hit regression
guard that runs on every CI of this crate.

Run:

```bash
soldr cargo bench -p fbuild-library-select --bench resolve_warm
```

Compare against `resolve_cold`: warm should be orders of magnitude faster
(K/V lookup + bincode decode vs. full filesystem walk + LDF reconciliation).
