# Toolchain Packages

Platform-specific toolchain management: download, cache, and provide tool paths for cross-compilation.

## Modules

- **`mod.rs`** -- Module root; re-exports `AvrToolchain`, `ArmToolchain`, `TeensyArmToolchain`, `Esp32Toolchain`, `Esp8266Toolchain`, `ClangComponent`
- **`avr.rs`** -- AVR-GCC 7.3.0 toolchain from Arduino's CDN
- **`arm.rs`** -- ARM GCC 15.2 toolchain (arm-none-eabi) from developer.arm.com
- **`teensy_arm.rs`** -- Teensy-pinned ARM GCC 11.3.1 package from PlatformIO/PJRC
- **`esp32.rs`** -- ESP32 RISC-V (`riscv32-esp-elf`) and Xtensa (`xtensa-esp-elf`) GCC from Espressif
- **`esp32_metadata.rs`** -- Resolves ESP32 toolchain URLs from `tools.json` metadata packages
- **`esp_idf_install.rs`** -- Installs a whole pinned ESP-IDF release (source + tools + Python) and returns the `PATH`/env a delegated `idf.py` build needs: the venv's `bin` leads `PATH`, `IDF_PATH` and `IDF_PYTHON_ENV_PATH` are exported, and `IDF_TOOLS_PATH` deliberately is not (#1970)
- **`esp_idf_python.rs`** -- Pins the ESP-IDF v5.5.1 *Python environment*: a python-build-standalone CPython 3.12 per host plus the release's `requirements.core.txt` resolved against its constraints into one hash-locked file, installed with `pip --require-hashes --no-deps` (#1970)
- **`esp_idf_release.rs`** -- Pinned ESP-IDF v5.5.1 *tool components* (Xtensa GCC, ULP binutils, ROM ELFs, OpenOCD) as fbuild packages, with an `Esp32S3Only` install profile; replaces `install.sh` provisioning (#1970)
- **`esp_idf_release_profile.rs`** -- Installs a whole `IdfProfile` and describes the environment a delegated `idf.py` build needs (PATH entries, `ESP_ROM_ELF_DIR`, `OPENOCD_SCRIPTS`); `installed_bytes()` measures the installed tree in `du` terms (hardlinks counted once) so the 1.3 GB budget is checked against disk (#1970)
- **`esp_idf_source.rs`** -- Pins the ESP-IDF v5.5.1 *source tree* as one fbuild package: the pinned commit's own archive plus all 23 gitlinks, each sha256-verified and filled by `SubmodulePlan`, so a delegated `idf.py` build has a real checkout with no `git` and no `install.sh` (#1970)
- **`esp8266.rs`** -- ESP8266 Xtensa LX106 GCC from esp-quick-toolchain releases
- **`clang.rs`** -- LLVM/Clang toolchain components (clang, clang-tidy, include-what-you-use)

- **`gcc.rs`** -- Queries the selected GCC C++ multilib and canonicalizes system-header search paths for relocated Windows toolchains (#1701).
