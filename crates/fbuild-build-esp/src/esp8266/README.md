# ESP8266 Build Support

Compiler, linker, and orchestrator for ESP8266 boards (NodeMCU, Wemos D1) using the Xtensa LX106 toolchain.

## Modules

- **`mod.rs`** -- Module root; `Esp8266PlatformSupport` implementing `PlatformSupport`
- **`esp8266_compiler.rs`** -- Xtensa LX106 GCC compiler; flags from `Esp8266McuConfig` JSON
- **`esp8266_linker.rs`** -- Links ESP8266 objects into firmware.elf, converts to firmware.bin
- **`elf2bin.rs`** -- Rust port of the core's `tools/elf2bin.py`: eboot + sketch flash image with CRC
- **`mcu_config.rs`** -- Data-driven MCU config from embedded JSON (single Xtensa LX106 variant)
- **`framework_libs.rs`** -- Bundled Arduino libraries (`ESP8266WiFi`, `ESP8266WebServer`, ...): resolves
  the ones the sketch reaches and adds their sources to the core compile set (#1994). Tests in `framework_libs_tests.rs`
- **`orchestrator.rs`** -- Wires config, packages, compiler, and linker into a full build pipeline
