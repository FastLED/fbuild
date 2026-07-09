# ESP32 Build Support

Platform-specific build orchestration for all ESP32 MCU variants.

## Modules

- `mcu_config` — Data-driven MCU configuration from embedded JSON files
- `esp32_compiler` — Compiler using RISC-V or Xtensa GCC, flags from MCU config
- `esp32_linker` — Linker with multiple scripts, 100+ precompiled libs, response files
- `orchestrator` — Build phases: config, packages, compile, link, convert

## Architecture

ESP32 uses a **data-driven** approach: per-MCU JSON configs (in `configs/`) are embedded
at compile time and parsed into `Esp32McuConfig` structs. All compiler/linker flags come
from these configs, not from hardcoded values.

Two toolchain families:
- **RISC-V** (`riscv32-esp-elf`): ESP32-C2, C3, C5, C6, H2, P4
- **Xtensa** (`xtensa-esp-elf`): ESP32, S2, S3
