# Toolchain Packages

Platform-specific toolchain management: download, cache, and provide tool paths for cross-compilation.

## Modules

- **`mod.rs`** -- Module root; re-exports `AvrToolchain`, `ArmToolchain`, `TeensyArmToolchain`, `Esp32Toolchain`, `Esp8266Toolchain`, `ClangComponent`
- **`avr.rs`** -- AVR-GCC 7.3.0 toolchain from Arduino's CDN
- **`arm.rs`** -- ARM GCC 15.2 toolchain (arm-none-eabi) from developer.arm.com
- **`teensy_arm.rs`** -- Teensy-pinned ARM GCC 11.3.1 package from PlatformIO/PJRC
- **`esp32.rs`** -- ESP32 RISC-V (`riscv32-esp-elf`) and Xtensa (`xtensa-esp-elf`) GCC from Espressif
- **`esp32_metadata.rs`** -- Resolves ESP32 toolchain URLs from `tools.json` metadata packages
- **`esp8266.rs`** -- ESP8266 Xtensa LX106 GCC from esp-quick-toolchain releases
- **`clang.rs`** -- LLVM/Clang toolchain components (clang, clang-tidy, include-what-you-use)
