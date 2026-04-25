# fbuild-library-select benches

Criterion benchmarks for the PlatformIO-LDF-style library resolver.

## resolve_cold

End-to-end cold-path measurement of `resolve()` against a synthetic
~30-library framework tree (Teensyduino-class) built with `MiniFramework`. A
5-deep transitive include chain forces the two-pass LDF reconciliation; the
remaining libraries are unreferenced and must be rejected — that doubles as a
guard against the #204 over-selection regression. Walks the tempdir on every
iteration since no cache sits in front of `resolve()` today (Phase 4
memoization waits on zccache#130).

The Phase 7 P-02 threshold from FastLED/fbuild#205 is **≤ 200 ms cold for a
typical teensy41 project**. This bench captures the baseline; future PRs gate
against it.

Run:

```bash
uv run soldr cargo bench -p fbuild-library-select --bench resolve_cold
```
