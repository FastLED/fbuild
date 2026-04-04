# SAM Platform Build Support

Build orchestrator for Atmel SAM boards (ARM Cortex-M3). Uses the ARM GCC toolchain and SAM Arduino cores.

## Modules

- **`mod.rs`** -- `SamPlatformSupport` implementing `PlatformSupport`
- **`orchestrator.rs`** -- `SamOrchestrator` wiring config, compiler, linker
- **`mcu_config.rs`** -- Data-driven MCU config from embedded JSON
- **`sam_compiler.rs`** -- ARM Cortex-M3 compiler implementation
- **`sam_linker.rs`** -- ARM Cortex-M3 linker implementation
- **`configs/`** -- Embedded JSON MCU configurations
