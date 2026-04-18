# TODO — Warm-pass perf investigation (#91)

## Plan

- [x] Add `perf_log` module in `fbuild-build` with env-gated (`FBUILD_PERF_LOG=1`) phase timer
- [x] Instrument `BuildContext::new()` (config parse, board load, build-dir setup, flag collect)
- [x] Instrument `pipeline::run_sequential_build_with_libs` phases (core, variant, sketch, libs, compiledb, link)
- [x] Instrument AVR orchestrator outer phases (toolchain, framework, scan)
- [x] Instrument daemon `build` handler (lock acquire, spawn_blocking bookkeeping)
- [x] Instrument CLI round-trip for the warm build path
- [x] Ensure `cargo check` + `cargo clippy` + `cargo fmt` clean
- [x] Run cold+warm experiment on `tests/platform/uno`
- [x] Write `docs/PERF_WARM_BUILD.md` with methodology, phase table, top stalls, follow-ups
- [x] Add row to `docs/INDEX.md`
- [x] Commit (no push)

## Review

See `docs/PERF_WARM_BUILD.md` for measurements + top stalls.
