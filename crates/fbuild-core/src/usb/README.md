# `fbuild_core::usb`

USB VID:PID → human-readable `(vendor, product)` resolution.

- `mod.rs` — public API surface (`resolve`, `try_resolve`, `pretty`, `install_online_cache`).
- `resolver.rs` — tiered lookup implementation + unit tests covering FTDI, CP210x, CH340, Espressif, and the synthetic fallback.
- `data.rs` — optional runtime overlay loaded from a JSON file. Used to pick up newly-assigned VID/PID pairs that the bundled `usb-ids` crate doesn't yet know about. Powered by the repo's `online-data` branch and its nightly refresh workflow.
