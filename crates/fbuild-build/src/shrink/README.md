# shrink

Flash-size reduction subsystem behind `fbuild build --shrink[=MODE]` and `--no-shrink`. Tracked in [FastLED/fbuild#493](https://github.com/FastLED/fbuild/issues/493).

## Status

Phase 0a scaffold: this directory exists but exports nothing. Subsequent phases land the real plumbing:

| Phase | Adds |
|---|---|
| Phase 1 | `ShrinkMode` enum, `ShrinkPlan`, fail-closed libc probe, per-platform `AutoShrinkEntry` registry (all entries empty), green reporting one-liner, build-info JSON record |
| Phase 2 | Vendored picolibc tinystdio sources (`printf_thin/`), `shadow_archive.rs` shim TUs, `libprintf_thin.a` build pipeline |
| Phase 3 | Per-newlib-version symbol manifest + CI drift-detection test |
| Phase 4 | `spec_emitter.rs` (`printf-thin.specs`), ESP32 link-orchestrator wiring, build-fingerprint augmentation, compile-many stage-1 amortization. First measurable shrink (ESP32-S3 default build drops ≥ 18 KB flash). |
| Phase 5 | `wrap_fallback.rs` — `-Wl,--wrap=` path for `--shrink=printf` single-knob debug mode |
| Phase 6 | Per-platform rollout: STM32, NRF52, RP2040/RP2350, ESP8266, ESP32-C3, AVR (one PR per platform) |
| Phase 7 | Aggressive mode appliers: `-Oz` (GCC ≥ 12), ESP32 sdkconfig stack knobs, `esp_err_msg_table` strip, coredump strip, personality drop |
| Phase 8 | `BREAKING_CHANGES.md`, `docs/SHRINK.md`, release notes |

## Design references

- [#493](https://github.com/FastLED/fbuild/issues/493) — final design and implementation phases
- [#492](https://github.com/FastLED/fbuild/issues/492) — design exploration (superseded by #493)
- [#491](https://github.com/FastLED/fbuild/issues/491) — `fbuild bloat`: emit and preserve `firmware.map` automatically (hard prerequisite for accurate shrink reporting)
