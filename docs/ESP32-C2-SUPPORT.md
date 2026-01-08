# ESP32-C2 Support in Zapio

## Overview

ESP32-C2 is now fully supported in Zapio v0.1.0+! This document explains how ESP32-C2 support works and why it required special implementation.

## Quick Start

```bash
# Build ESP32-C2 firmware
zap build tests/esp32c2 -e esp32c2

# Deploy to device
zap deploy tests/esp32c2 -e esp32c2 --port COM3

# Monitor serial output
zap monitor tests/esp32c2 -e esp32c2
```

## Why ESP32-C2 is Special

ESP32-C2 is Espressif's cost-optimized microcontroller with:
- **Limited Flash**: Typically 2MB (vs 4MB+ on other ESP32 variants)
- **ROM Functions**: Many functions pre-programmed in chip ROM to save flash
- **No Standard Libraries**: Arduino doesn't provide standard precompiled libraries
- **Skeleton Library Approach**: Uses special "skeleton" libraries with ROM linker scripts

## Technical Architecture

### ROM Functions

Unlike other ESP32 chips, ESP32-C2 stores these functions in ROM:

1. **Hardware Abstraction Layer (HAL)**
   - `wdt_hal_*` - Watchdog timer functions
   - `systimer_hal_*` - System timer functions

2. **Heap Management**
   - `multi_heap_*` - Dynamic memory allocation
   - `tlsf_*` - Two-Level Segregate Fit allocator

3. **Standard Library**
   - Newlib nano implementation
   - Printf/scanf floating point support

4. **System Functions**
   - FreeRTOS primitives
   - Logging infrastructure
   - Bluetooth stack basics

### Skeleton Library

The skeleton library provides:
```
tools/sdk/esp32c2/
├── ld/           # ROM linker scripts (critical!)
│   ├── esp32c2.rom.wdt.ld
│   ├── esp32c2.rom.systimer.ld
│   ├── esp32c2.rom.heap.ld
│   └── ... (more ROM scripts)
├── lib/          # Pre-compiled ESP-IDF libraries (96 files)
│   ├── libfreertos.a
│   ├── libesp_system.a
│   └── ...
└── include/      # ESP-IDF headers
    └── ... (all necessary headers)
```

### Build Pipeline

```
1. Download skeleton library from GitHub
   └─> Extract to .zap/cache/platforms/

2. Compile Arduino core (55 source files)
   ├─> Uses skeleton headers
   └─> Produces core.a (4MB)

3. Compile user sketch
   └─> Produces sketch.o

4. Link everything
   ├─> core.a
   ├─> 96 ESP-IDF libraries
   ├─> 16 linker scripts (including ROM scripts)
   └─> Produces firmware.elf

5. Convert to binary
   ├─> firmware.elf → firmware.bin (220KB typical)
   ├─> Generate bootloader.bin
   └─> Generate partitions.bin
```

## Configuration

The ESP32-C2 platform configuration is in `src/zapio/platform_configs/esp32c2.json`.

### Key Configuration Elements

**ROM Linker Scripts:**
```json
"linker_scripts": [
  "esp32c2.rom.wdt.ld",        // Watchdog HAL in ROM
  "esp32c2.rom.systimer.ld",   // System timer HAL in ROM
  "esp32c2.rom.heap.ld",       // Heap functions in ROM
  "esp32c2.rom.rvfp.ld",       // RISC-V floating point
  "esp32c2.rom.newlib-nano.ld",// Newlib nano
  "esp32c2.rom.ble.ld",        // Bluetooth
  // ... more scripts
]
```

**Special Linker Flags:**
```json
"-Wl,--defsym=__wrap_log_printf=esp_log_printf", // Log wrapper alias
"--specs=nano.specs",                             // Use newlib-nano
"-u", "tlsf_set_rom_patches",                    // Force ROM patches
"-u", "esp_rom_include_multi_heap_patch"         // Force heap patch
```

## platformio.ini Configuration

```ini
[env:esp32c2]
platform = https://github.com/pioarduino/platform-espressif32.git#develop
board = esp32-c2-devkitm-1
framework = arduino
monitor_speed = 115200

; Optional: Add build flags
build_flags =
    -DCORE_DEBUG_LEVEL=3

; Optional: Libraries
lib_deps =
    https://github.com/user/library
```

## Build Performance

Typical build metrics:
- **Clean Build**: ~60-65 seconds
- **Incremental Build**: ~5-10 seconds (minimal changes)
- **Firmware Size**: 220KB (Blink sketch)
- **Core Archive**: 4MB (includes all Arduino functions)
- **Total ELF**: ~5MB (before stripping debug symbols)

## Troubleshooting

### "undefined reference to wdt_hal_*"

**Cause**: Missing ROM linker script `esp32c2.rom.wdt.ld`

**Solution**: This should be fixed in v0.1.0+. If you see this error, ensure you're using the latest version.

### "undefined reference to __wrap_log_printf"

**Cause**: Missing log wrapper alias

**Solution**: This should be fixed in v0.1.0+. The linker flag `--defsym=__wrap_log_printf=esp_log_printf` aliases the wrapper.

### Build takes very long on first run

**Cause**: Downloading skeleton library and toolchain

**Solution**: Normal behavior. Subsequent builds use cached packages.

### Firmware too large

**Cause**: Debug symbols included, optimization level too low

**Solution**:
- Debug builds include symbols (normal)
- Release builds strip symbols automatically
- Adjust `build_flags` in platformio.ini for more optimization

## Differences from Other ESP32 Variants

| Feature | ESP32/C3/S3 | ESP32-C2 |
|---------|-------------|----------|
| Library Source | Precompiled release libs | Skeleton library |
| HAL Functions | In static libraries | In chip ROM |
| Linker Scripts | Standard scripts | ROM scripts required |
| Flash Size | 4MB+ typical | 2MB typical |
| Build Complexity | Standard | Requires special config |

## Known Limitations

1. **No Board Manager Support**: ESP32-C2 is not available through Arduino Board Manager. Must use PlatformIO-style platform URLs.

2. **Requires Develop Branch**: Uses pioarduino develop branch, not release versions.

3. **No Prebuilt Libraries**: Unlike other ESP32 variants, C2 requires skeleton library approach.

4. **Limited Documentation**: ESP32-C2 is less documented than other ESP32 variants.

## References

- [ESP32 Arduino Lib Builder](https://github.com/espressif/esp32-arduino-lib-builder)
- [Arduino ESP32 Documentation](https://docs.espressif.com/projects/arduino-esp32/en/latest/)
- [PlatformIO ESP32 Platform](https://github.com/pioarduino/platform-espressif32)
- [ESP32-C2 Datasheet](https://www.espressif.com/sites/default/files/documentation/esp8685_datasheet_en.pdf)
- [ESP32-C2 Technical Reference](https://www.espressif.com/sites/default/files/documentation/esp8685_technical_reference_manual_en.pdf)

## Implementation Details

For developers working on Zapio itself, see `.agent_task/LOOP.md` for complete implementation details and the solution process.

## Version History

- **v0.1.0** (2026-01-08): ESP32-C2 support added
  - Skeleton library integration
  - ROM linker script support
  - Full Arduino framework compatibility
  - Successful firmware builds

---

**Status**: ✅ Fully Supported
**Since**: v0.1.0
**Test Platform**: esp32-c2-devkitm-1
