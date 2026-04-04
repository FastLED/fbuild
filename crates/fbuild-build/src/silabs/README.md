# Silicon Labs Platform Build Support

Build orchestrator for Silicon Labs boards (ARM Cortex-M33). Uses the ARM GCC toolchain and Silicon Labs Arduino cores.

## Modules

- **`mod.rs`** -- `SilabsPlatformSupport` implementing `PlatformSupport`
- **`orchestrator.rs`** -- `SilabsOrchestrator` wiring config, compiler, linker
- **`mcu_config.rs`** -- Data-driven MCU config from embedded JSON
- **`silabs_compiler.rs`** -- ARM Cortex-M33 compiler implementation
- **`silabs_linker.rs`** -- ARM Cortex-M33 linker implementation
- **`configs/`** -- Embedded JSON MCU configurations
