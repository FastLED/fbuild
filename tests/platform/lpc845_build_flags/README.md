# `lpc845_build_flags`

Regression fixture for FastLED/fbuild#587 — proves that
`[env:lpc845brk] build_flags = -D…` in `platformio.ini` reach the nxplpc
orchestrator's **library** compile path (not just the sketch path).

## Layout

- `platformio.ini` — sets `build_flags = -DFROM_PLATFORMIO_INI=1` and routes
  the library compile through `lib_extra_dirs = libs`.
- `src/main.ino` — minimal Arduino sketch; calls a no-op symbol from the
  probe library so it actually gets linked.
- `libs/check_flag/src/check_flag.cpp` — `#error`s out unless
  `FROM_PLATFORMIO_INI` is defined.

Build succeeds → orchestrator now propagates `build_flags` to libraries.

## Driver

The driver lives at `crates/fbuild-build/tests/nxplpc_build_flags.rs` and is
marked `#[ignore]` because it downloads the ARM GCC toolchain plus the vendored
ArduinoCore-LPC8xx and performs a real link. Invoke with:

```
soldr cargo test -p fbuild-build --test nxplpc_build_flags -- --ignored
```

See also: `tests/platform/lpc845/` (the pre-existing Blink fixture, no
`build_flags` propagation assertion attached).
