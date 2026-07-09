# NRF52 Platform Build Support

Build orchestrator for Nordic NRF52 boards (ARM Cortex-M4F). Uses the ARM GCC toolchain and NRF52 Arduino cores.

## Modules

- **`mod.rs`** -- `Nrf52PlatformSupport` implementing `PlatformSupport`
- **`orchestrator.rs`** -- `Nrf52Orchestrator` wiring config, compiler, linker
- **`mcu_config.rs`** -- Data-driven MCU config from embedded JSON
- **`nrf52_compiler.rs`** -- ARM Cortex-M4F compiler implementation
- **`nrf52_linker.rs`** -- ARM Cortex-M4F linker implementation
- **`configs/`** -- Embedded JSON MCU configurations
