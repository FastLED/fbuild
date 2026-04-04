# AVR Build Support

Compiler, linker, and orchestrator for AVR ATmega boards (Arduino Uno, Mega, Nano).

## Modules

- **`mod.rs`** -- Module root; `AvrPlatformSupport` implementing `PlatformSupport`
- **`avr_compiler.rs`** -- AVR-GCC compiler; compiles C/C++ sources with `-mmcu=` flags
- **`avr_linker.rs`** -- Links AVR objects into firmware.elf, converts to firmware.hex
- **`mcu_config.rs`** -- Data-driven MCU config from embedded JSON (ATmega328P, ATmega2560, etc.)
- **`orchestrator.rs`** -- Wires config, packages, compiler, and linker into a full build pipeline
