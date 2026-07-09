# esp32_framework

ESP32 Arduino framework package manager. Split into submodules to keep each
file below the 1000 LOC gate.

## Layout

- `mod.rs` — `Esp32Framework` struct, constructors, `Package`/`Framework` trait
  impls, and the legacy `validate` helper.
- `libs.rs` — `ensure_libs` / `ensure_mcu_libs` (download + extract the ESP-IDF
  SDK and per-MCU skeleton libs into `tools/`).
- `paths.rs` — Simple framework path accessors (cores, variants, bootloader,
  partitions, gen_esp32part.py, core sources).
- `sdk_paths.rs` — SDK include directories, library flags, defines, linker
  flags, and linker-script flags. Includes the `sdk_mcu_dir` and memory-variant
  resolver.
- `parsing.rs` — Tokenizers for the SDK `flags/*` files (`-D` defines and
  `-I` / `-iwithprefixbefore` include flags) and URL version extraction.
- `fs_utils.rs` — Generic filesystem helpers: recursive copy, framework-root
  detection, recursive include scanner, archive/source collectors.
- `tests.rs` — Unit tests for the module (cfg(test) only).

External API surface (`Esp32Framework` and its inherent / trait methods) is
unchanged. The only re-export from the parent module remains
`crate::library::Esp32Framework`.
