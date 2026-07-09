# Renesas RA Platform Build Support

Build orchestrator for Renesas RA boards (ARM Cortex-M4). Uses the ARM GCC toolchain and Renesas Arduino cores.

## Modules

- **`mod.rs`** -- `RenesasPlatformSupport` implementing `PlatformSupport`
- **`orchestrator.rs`** -- `RenesasOrchestrator` wiring config, compiler, linker
- **`mcu_config.rs`** -- Data-driven MCU config from embedded JSON
- **`renesas_compiler.rs`** -- ARM Cortex-M4 compiler implementation
- **`renesas_linker.rs`** -- ARM Cortex-M4 linker implementation
- **`configs/`** -- Embedded JSON MCU configurations
