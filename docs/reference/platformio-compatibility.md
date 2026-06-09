# PlatformIO Compatibility

fbuild is designed to consume existing `platformio.ini` projects while using a
Rust-native build, deploy, and monitor pipeline. This page owns compatibility
notes that are too detailed for the root README.

## `.eh_frame` Strip Policy

By default, fbuild strips GCC's `.eh_frame` exception-unwinding tables on
release builds for platforms that do not use them: Teensy, STM32, RP2040,
NRF52, and ESP8266. This can save 40-180 KB of flash on a typical FastLED
sketch. PlatformIO does not strip these tables by default.

ESP32 with the stock Arduino sdkconfig preserves `.eh_frame` because
`esp32_exception_decoder` and panic-print-backtrace consume it.

The decision is made per build by this precedence chain:

| Condition | Policy |
|---|---|
| `FBUILD_STRIP_EH_FRAME=1` env var | Strip |
| `FBUILD_KEEP_EH_FRAME=1` env var | Preserve |
| `build_type = debug` in `platformio.ini` | Preserve |
| `-fexceptions`, `-funwind-tables`, or `-fasynchronous-unwind-tables` in `build_flags` | Preserve |
| ESP32 with `CONFIG_ESP_SYSTEM_PANIC_PRINT_BACKTRACE=y` | Preserve |
| Otherwise, on supported release platforms | Strip |

## Opt Out Per Project

Add an unwind-tables flag to `platformio.ini`:

```ini
[env:teensy41]
platform = teensy
board = teensy41
framework = arduino
build_flags = -funwind-tables
```

Or set an environment variable:

```bash
FBUILD_KEEP_EH_FRAME=1 fbuild build
```

## Why fbuild Deviates

GCC emits `.eh_frame` by default even when nothing consumes it. On the
platforms above, toolchain JSON already ships `-fno-exceptions`, no runtime
calls `_Unwind_*`, and no debugger or decoder reads the tables. In that case
`.eh_frame` is dead metadata occupying flash.

A byte-level audit on an ESP32-S3 FastLED Blink build
([FastLED/FastLED#2473](https://github.com/FastLED/FastLED/issues/2473)) found
`.eh_frame` accounted for 36% of firmware size. Stripping at the compiler level
with `-fno-asynchronous-unwind-tables -fno-unwind-tables` is the reliable fix.
Implementation details and the original decision matrix are in
[#245](https://github.com/fastled/fbuild/pull/245).

## PlatformIO-Compatible CI

`fbuild ci` is a drop-in replacement for `pio ci` for supported workflows. See
the [`fbuild ci` reference](cli.md#fbuild-ci) for flag mapping and examples.
