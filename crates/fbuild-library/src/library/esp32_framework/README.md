# esp32_framework

ESP32 Arduino framework package manager. Split into submodules to keep each
file below the 1000 LOC gate.

## Layout

- `mod.rs` — `Esp32Framework` struct, constructors, `Package`/`Framework` trait
  impls, and the legacy `validate` helper.
- `libs.rs` — `ensure_libs` / `ensure_mcu_libs` (download + extract the ESP-IDF
  SDK and per-MCU skeleton libs into `tools/`), and the component identities of
  the SDK variant and the MCU skeleton: compat patches run in the extract dir,
  the stamp and its content id are taken there, and the merge re-checks the id
  before the stamp is allowed to stay. A stamped tree is never patched in place
  (decision 8a, FastLED/fbuild#1882).
- `identity.rs` — the framework term: reads the `core`, `sdk-<variant>` and
  `mcu-<mcu>` stamps of an installed tree into one canonical id, or `None`
  when any required component has no identity. Stamps only — never a tree walk
  (decision 8a, FastLED/fbuild#1882).
- `prebuilt_core.rs` — the canonical prebuilt-core fingerprint: hashes the ABI
  schema, toolchain, framework term, MCU/SDK variant, the canonical argv of every
  compiled unit and the config/header digests into one key, or `None` when the
  tree has no identity or an input is missing. Install-history and
  install-path independent; never writes a stamp (decision 8, FastLED/fbuild#1882).
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
`crate::library::Esp32Framework`; `prebuilt_core` is a public module so a
caller can name `PrebuiltCorePlan` and the input structs explicitly.
