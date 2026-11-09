# Toolchain Packages

Platform-specific toolchain management: download, cache, and provide tool paths for cross-compilation.

## Modules

- **`mod.rs`** -- Module root; re-exports `AvrToolchain`, `ArmToolchain`, `TeensyArmToolchain`, `Esp32Toolchain`, `Esp8266Toolchain`, `ClangComponent`
- **`avr.rs`** -- AVR-GCC 7.3.0 toolchain from Arduino's CDN
- **`arm.rs`** -- ARM GCC 15.2 toolchain (arm-none-eabi) from developer.arm.com
- **`teensy_arm.rs`** -- Teensy-pinned ARM GCC 11.3.1 package from PlatformIO/PJRC
- **`esp32.rs`** -- ESP32 RISC-V (`riscv32-esp-elf`) and Xtensa (`xtensa-esp-elf`) GCC from Espressif
- **`esp32_metadata.rs`** -- Resolves ESP32 toolchain URLs from `tools.json` metadata packages
- **`esp_idf_release.rs`** -- Pinned ESP-IDF v5.5.1 *tool components* (Xtensa GCC, ULP binutils, ROM ELFs, OpenOCD) as fbuild packages, with an `Esp32S3Only` install profile; replaces `install.sh` provisioning (#1970)
- **`esp_idf_release_profile.rs`** -- Installs a whole `IdfProfile` and describes the environment a delegated `idf.py` build needs (PATH entries, `ESP_ROM_ELF_DIR`, `OPENOCD_SCRIPTS`); `installed_bytes()` measures the installed tree in `du` terms (hardlinks counted once) so the 1.3 GB budget is checked against disk (#1970)
- **`esp8266.rs`** -- ESP8266 Xtensa LX106 GCC from esp-quick-toolchain releases
- **`clang.rs`** -- LLVM/Clang toolchain components (clang, clang-tidy, include-what-you-use)

- **`gcc.rs`** -- Queries the selected GCC C++ multilib and canonicalizes system-header search paths for relocated Windows toolchains (#1701).
