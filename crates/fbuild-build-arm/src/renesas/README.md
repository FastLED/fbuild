# Renesas RA Platform Build Support

Build orchestrator for Renesas RA boards (ARM Cortex-M4 and Cortex-M33). Uses the ARM GCC toolchain and
the two Arduino Renesas core packages: `arduino:renesas_uno` for the RA4M1 boards (UNO R4 Minima/WiFi,
Nano R4) and `arduino:renesas_portenta` for the RA6M5 Portenta C33. The board's MCU selects both the
core package and the MCU config (`renesas_mcu_family`, FastLED/fbuild#2054).

## Modules

- **`mod.rs`** -- `RenesasPlatformSupport` implementing `PlatformSupport`
- **`orchestrator.rs`** -- `RenesasOrchestrator` wiring config, compiler, linker
- **`mcu_config.rs`** -- Data-driven MCU config from embedded JSON; `renesas_mcu_family` maps a board's
  `mcu` (`RA4M1`, `R7FA6M5BH2CBG`) to the family whose config and core package fbuild pins
- **`renesas_compiler.rs`** -- ARM Cortex-M4/M33 compiler implementation
- **`renesas_linker.rs`** -- ARM Cortex-M4/M33 linker implementation; flash in the size report leaves out `.id_code` (the ID-code region at 0x01010018, not code flash), matching PlatformIO renesas-ra (FastLED/fbuild#2042)
- **`framework_libs.rs`** -- Bundled Arduino libraries the sketch reaches (`WiFiS3`, `SPI`, `Wire`, ...):
  resolves the selection with the shared library-dependency finder and contributes their include dirs
  and sources to the compile set (FastLED/fbuild#2020). Tests in `framework_libs_tests.rs`
- **`configs/`** -- Embedded JSON MCU configurations
