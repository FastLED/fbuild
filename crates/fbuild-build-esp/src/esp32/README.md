# ESP32 Build Support

Platform-specific build orchestration for all ESP32 MCU variants.

## Modules

- `mcu_config` — Data-driven MCU configuration from embedded JSON files
- `esp32_compiler` — Compiler using RISC-V or Xtensa GCC, flags from MCU config
- `esp32_linker` — Linker with multiple scripts, 100+ precompiled libs, response files
- `firmware_bin_cache` — Cross-project reuse of the `elf2image` product, keyed on
  ELF contents plus the flash parameters (FastLED/fbuild#1894)
- `orchestrator` — Build phases: config, packages, compile, link, convert
- `espidf_delegate` — The opt-in delegated build (`board_build.build_system =
  espidf`): runs the pinned ESP-IDF release's own `idf.py` against the
  project's `CMakeLists.txt`, supervised as a subprocess (FastLED/fbuild#1971)
- `espidf_artifacts` — Discovers a delegated build's artifacts from IDF's own
  `flasher_args.json`, publishes them flat under `<build>/idf/`, and records
  `idf-artifacts.json` as the artifact proof

## Architecture

ESP32 uses a **data-driven** approach: per-MCU JSON configs (in `configs/`) are embedded
at compile time and parsed into `Esp32McuConfig` structs. All compiler/linker flags come
from these configs, not from hardcoded values.

Two toolchain families:
- **RISC-V** (`riscv32-esp-elf`): ESP32-C2, C3, C5, C6, H2, P4
- **Xtensa** (`xtensa-esp-elf`): ESP32, S2, S3

## Two build systems

The default path compiles sketches against the Arduino core. A project that
sets `board_build.build_system = espidf` instead has its own
`idf_component_register()` CMake project built by the pinned ESP-IDF release;
`orchestrator/build.rs` branches to `espidf_delegate` immediately after the
common `BuildContext`, so none of the Arduino package/framework work runs.
