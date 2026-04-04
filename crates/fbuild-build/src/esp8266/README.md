# ESP8266 Build Support

Compiler, linker, and orchestrator for ESP8266 boards (NodeMCU, Wemos D1) using the Xtensa LX106 toolchain.

## Modules

- **`mod.rs`** -- Module root; `Esp8266PlatformSupport` implementing `PlatformSupport`
- **`esp8266_compiler.rs`** -- Xtensa LX106 GCC compiler; flags from `Esp8266McuConfig` JSON
- **`esp8266_linker.rs`** -- Links ESP8266 objects into firmware.elf, converts to firmware.bin
- **`mcu_config.rs`** -- Data-driven MCU config from embedded JSON (single Xtensa LX106 variant)
- **`orchestrator.rs`** -- Wires config, packages, compiler, and linker into a full build pipeline
