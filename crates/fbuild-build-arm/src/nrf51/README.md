# NRF51 Platform Build Support

Build orchestrator for Nordic nRF51 boards (ARM Cortex-M0, no FPU), e.g. the BBC micro:bit. Uses the shared ARM GCC toolchain and the sandeepmistry arduino-nRF5 core (`framework-arduinonordicnrf5`), mirroring PlatformIO's `platform-nordicnrf51` Arduino builder. Links the board's no-SoftDevice script (e.g. `nrf51_xxaa.ld`).

## Modules

- **`mod.rs`** -- `Nrf51PlatformSupport` implementing `PlatformSupport`
- **`orchestrator.rs`** -- `Nrf51Orchestrator` wiring packages, sources, and the `generic_arm` compiler/linker
- **`mcu_config.rs`** -- Data-driven MCU config from embedded JSON
- **`configs/`** -- Embedded JSON MCU configurations
