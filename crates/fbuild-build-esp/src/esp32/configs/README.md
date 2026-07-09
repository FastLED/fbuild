# ESP32 MCU Configurations

Per-MCU JSON config files extracted from the Python fbuild reference implementation.
These are embedded at compile time via `include_str!()` in `mcu_config.rs`.

Each JSON file contains compiler flags, linker flags, linker scripts, defines,
profile settings, and esptool flash offsets for one ESP32 MCU variant.

## MCU Variants

| File | Architecture | Notes |
|---|---|---|
| `esp32.json` | Xtensa | Original ESP32, `-mlongcalls` |
| `esp32c2.json` | RISC-V | `rv32imc`, nano specs |
| `esp32c3.json` | RISC-V | `rv32imc` |
| `esp32c5.json` | RISC-V | `rv32imac` (atomics) |
| `esp32c6.json` | RISC-V | `rv32imac` (atomics) |
| `esp32p4.json` | RISC-V | `rv32imafc` (FPU), `ilp32f` ABI |
| `esp32s3.json` | Xtensa | `-mlongcalls`, no HW atomics |
