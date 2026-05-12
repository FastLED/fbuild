# `fbuild-library-select` integration tests

Tests that exercise the public `resolve()` / `resolve_with_stats()` API end-to-end
on tempfile-built mini-frameworks. Unit tests live under `src/`.

- `perf_tdd.rs` — gates for #236 (parallel walker + scan memoization + tracing
  spans). Asserts each reachable file is read exactly once across all passes
  and that `ldf_pass` / `ldf_walk` spans are emitted.
