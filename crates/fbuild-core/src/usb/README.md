# `fbuild_core::usb`

USB VID:PID → human-readable `(vendor, product)` resolution.

- `mod.rs` — public API surface (`resolve`, `try_resolve`, `pretty`, `install_online_cache`).
- `resolver.rs` — tiered lookup implementation + unit tests covering FTDI, CP210x, CH340, Espressif, and the synthetic fallback.
- `data.rs` — verified runtime catalogue cache populated from the published FastLED/boards artifacts.
- `profiles.rs` — typed board, runtime, bootloader, probe, and compile identity profiles from FastLED/boards.
