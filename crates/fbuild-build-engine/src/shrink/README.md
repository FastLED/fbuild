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

## `--shrink=printf` on ESP32 (measured)

`--shrink=printf` links the image against the newlib-nano copy of libc that
ships inside the *same* Espressif toolchain (`-specs=nano.specs`, plus
`-Wl,-u,_printf_float` so `%f`/`%e`/`%g` keep working). No formatter is
vendored here: the vendor's thin `_vfprintf_r`/`_svfprintf_r` replace the full
ones at link time, and the decision is reported in the build log and in
`build_info_<env>.json`.

### It is Xtensa-only, because that is where it shrinks

Same source (the printf battery in
`crates/fbuild-build/tests/it/esp32_shrink_printf.rs`), same platform
(`espressif32`), `--no-shrink` vs `--shrink=printf`, fresh dirs, this host:

| board | arch / toolchain | `--no-shrink` flash | `--shrink=printf` flash | delta |
|---|---|---|---|---|
| `esp32dev` | Xtensa LX6, xtensa-esp32-elf 8.4.0 | 277,185 B | 254,036 B | **−23,149 B (−8.4 %)** |
| `esp32s3` | Xtensa LX7, xtensa-esp32s3-elf 8.4.0 | 328,162 B | 305,134 B | **−23,028 B (−7.0 %)** |
| `esp32c6` | RISC-V, riscv32-esp-elf 14.2.0 | 280,848 B | 290,374 B | **+9,526 B** |

The RISC-V result is why `resolve_shrink` asks `mcu_config.is_xtensa()`: on
ESP32-C6 the full formatter is only ~15 KB (9,658 B `_vfprintf_r` + 5,740 B
`_vfiprintf_r`), while nano's replacement drags `_dtoa_r` (3,642 B) and
`_printf_float` (1,346 B) back into `.flash.text` with an offsetting +8.3 KB
of other libc code — a net growth. A reducer that grows the image must never
be applied, so RISC-V targets build unshrunk and print one always-visible
`WARNING: shrink: --shrink=printf is Xtensa-only …` line instead
(`build_info` records `resolved: off`, `applied: []`, so terminal and JSON
agree).

### Semantic coverage

The battery in `tests/it/esp32_shrink_printf.rs` formats integers (`%d %u %ld
%lld %llu %x %o %zu` and the `h` modifiers), width/precision/flags
(`%08d %-6d %+d %#x %*d %.*f` and `%.0d`), floats (`%f %e %g`, precision,
`inf`) and the `%s %c %%` conversions through `snprintf`, `sprintf` and
`vsnprintf` (what `Serial.printf` funnels through), comparing each result to a
literal expectation and printing `BATTERY total=N failed=M` over serial. Two
`--ignored` integration tests build it both ways and assert the flash/telemetry
halves; flashing either image answers the formatting half on real silicon.

Known divergences, deliberately not asserted: nano has no wide-character or
`long double` conversions.

`Auto` still resolves to `Off` for every platform. Flipping ESP32's `Auto` to
this mode needs the guarded A/B report *and* the hardware formatting run per
board family — the C6 measurement above is exactly why that stays opt-in.
