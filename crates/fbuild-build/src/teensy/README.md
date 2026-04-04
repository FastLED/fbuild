# Teensy Build Support

Compiler, linker, and orchestrator for Teensy boards (4.0, 4.1, 3.x, LC) using the ARM arm-none-eabi toolchain.

## Modules

- **`mod.rs`** -- Module root; `TeensyPlatformSupport` implementing `PlatformSupport`
- **`teensy_compiler.rs`** -- ARM Cortex-M7 GCC compiler for Teensy boards
- **`teensy_linker.rs`** -- Links ARM objects into firmware.elf, converts to firmware.hex
- **`mcu_config.rs`** -- Data-driven MCU config from embedded JSON (Teensy 4.x, 3.x, LC variants)
- **`orchestrator.rs`** -- Wires config, packages, compiler, and linker into a full build pipeline
