# NRF52 Platform Build Support

Build orchestrator for Nordic NRF52 boards (ARM Cortex-M4F). Uses the ARM GCC toolchain and NRF52 Arduino cores: Adafruit's nRF52 core by default, Seeed's fork of it for boards whose JSON sets `build.framework_package = framework-arduinoadafruitnrf52-seeed`.

## Modules

- **`mod.rs`** -- `Nrf52PlatformSupport` implementing `PlatformSupport`; `installed_cores` gives `fbuild lib-select` the same core the build selects (FastLED/fbuild#2042)
- **`orchestrator.rs`** -- `Nrf52Orchestrator` wiring config, compiler, linker
- **`core_select.rs`** -- Picks the board's Adafruit-family core (Adafruit's, or Seeed's fork for the XIAO nRF52840 boards) from board JSON `build.framework_package`, and the SoftDevice API headers matching its linker script (FastLED/fbuild#2017); tests in `core_select_tests.rs`
- **`mcu_config.rs`** -- Data-driven MCU config from embedded JSON
- **`nrf52_compiler.rs`** -- ARM Cortex-M4F compiler implementation
- **`nrf52_linker.rs`** -- ARM Cortex-M4F linker implementation
- **`configs/`** -- Embedded JSON MCU configurations
