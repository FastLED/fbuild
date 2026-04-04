![fbuild](https://github.com/user-attachments/assets/7db78eba-b10f-44c7-ae32-7fc0b5e46642)

*A fast, next-generation multi-platform compiler, deployer, and monitor for embedded development, directly compatible with platformio.ini*

## Build Status

### CI
[![Check Ubuntu](https://github.com/fastled/fbuild/actions/workflows/check-ubuntu.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/check-ubuntu.yml)
[![Check macOS](https://github.com/fastled/fbuild/actions/workflows/check-macos.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/check-macos.yml)
[![Check Windows](https://github.com/fastled/fbuild/actions/workflows/check-windows.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/check-windows.yml)
[![Formatting](https://github.com/fastled/fbuild/actions/workflows/fmt.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/fmt.yml)
[![Min Rust Version](https://github.com/fastled/fbuild/actions/workflows/msrv.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/msrv.yml)
[![Documentation](https://github.com/fastled/fbuild/actions/workflows/docs.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/docs.yml)
[![Validate Boards](https://github.com/fastled/fbuild/actions/workflows/validate-boards.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/validate-boards.yml)

### Native Binaries
[![Build Native Binaries](https://github.com/fastled/fbuild/actions/workflows/build.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build.yml)

### AVR
[![Build Arduino Uno](https://github.com/fastled/fbuild/actions/workflows/build-uno.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-uno.yml)
[![Build Leonardo](https://github.com/fastled/fbuild/actions/workflows/build-leonardo.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-leonardo.yml)

### ESP8266
[![Build ESP8266](https://github.com/fastled/fbuild/actions/workflows/build-esp8266.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-esp8266.yml)

### ESP32
[![Build ESP32 Dev](https://github.com/fastled/fbuild/actions/workflows/build-esp32dev.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-esp32dev.yml)
[![Build ESP32-C2](https://github.com/fastled/fbuild/actions/workflows/build-esp32c2.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-esp32c2.yml)
[![Build ESP32-C3](https://github.com/fastled/fbuild/actions/workflows/build-esp32c3.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-esp32c3.yml)
[![Build ESP32-C5](https://github.com/fastled/fbuild/actions/workflows/build-esp32c5.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-esp32c5.yml)
[![Build ESP32-C6](https://github.com/fastled/fbuild/actions/workflows/build-esp32c6.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-esp32c6.yml)
[![Build ESP32-H2](https://github.com/fastled/fbuild/actions/workflows/build-esp32h2.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-esp32h2.yml)
[![Build ESP32-P4](https://github.com/fastled/fbuild/actions/workflows/build-esp32p4.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-esp32p4.yml)
[![Build ESP32-S2](https://github.com/fastled/fbuild/actions/workflows/build-esp32s2.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-esp32s2.yml)
[![Build ESP32-S3](https://github.com/fastled/fbuild/actions/workflows/build-esp32s3.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-esp32s3.yml)

### Teensy
[![Build Teensy 4.1](https://github.com/fastled/fbuild/actions/workflows/build-teensy41.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-teensy41.yml)
[![Build Teensy 4.0](https://github.com/fastled/fbuild/actions/workflows/build-teensy40.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-teensy40.yml)
[![Build Teensy 3.6](https://github.com/fastled/fbuild/actions/workflows/build-teensy36.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-teensy36.yml)
[![Build Teensy LC](https://github.com/fastled/fbuild/actions/workflows/build-teensylc.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-teensylc.yml)



# fbuild

`fbuild` is a next-generation embedded development tool featuring a clean extensible data driven architecture. It provides fast incremental builds, URL-based package management, and soon to be comprehensive multi-platform support for Arduino and ESP32 development.

**platformio.ini compatible**

fbuild uses the same `platformio.ini` already used in platformio sketches.



**TODO: firmware.bin size comparisons between Arduino/PlatformIO vs fbuild**

**Design Goals**

  * Replaces `platformio` in `FastLED` repo builders
  * Correct and blazing parallel package management system
    * locking is done through a daemon process
    * packages are fingerprinted to their version and cached, download only once
    * zccache for caching compiles
  * Easily add features via AI
    * This codebase is designed and implemented by AI, just fork it and ask ai to make your change Please send us a PR!
  * Supports new build chains easily
  * Supports wasm builds natively
    
**Current Status**: v2.0.6 - Rust rewrite. Full ESP32 / Teensy support with working build system

## Examples

**Quick start** - Build, deploy, and monitor in one command:

```bash
# install
pip install fbuild
```

```bash
# Default action: build + deploy
fbuild tests/platform/esp32c6
```

```bash
# Default action: build + deploy + monitor
fbuild tests/platform/esp32c6 --monitor
```


**Deploy commands:**

```bash
# Deploy with clean build
fbuild deploy tests/platform/esp32c6 --clean

# Deploy with monitoring and test patterns
fbuild deploy tests/platform/esp32c6 --monitor="--timeout 60 --halt-on-error \"TEST FAILED\" --halt-on-success \"TEST PASSED\""
```

**Monitor command:**

```bash
# Monitor serial output with pattern matching
fbuild monitor --timeout 60 --halt-on-error "TEST FAILED" --halt-on-success "TEST PASSED"
```

  * Serial monitoring requires pyserial to attach to the USB device
  * Port auto-detection works similarly to PlatformIO

## QEMU Testing

fbuild supports deploying to QEMU for testing ESP32 firmware without physical hardware.

### QEMU Supported Platforms

| Platform | QEMU Status | Notes |
|----------|-------------|-------|
| ESP32dev (original ESP32) | ✅ Fully supported | Recommended for QEMU testing |
| ESP32-S3 | ❌ Not supported | Bootloader incompatible with QEMU |
| ESP32C6 | ❌ Not supported | QEMU lacks C6 emulation |
| ESP32C3 | ⚠️ Untested | May work (RISC-V architecture) |

### Usage

```bash
# Build for QEMU (use esp32dev)
fbuild build tests/platform/esp32dev -e esp32dev-qemu

# Deploy to QEMU
fbuild deploy tests/platform/esp32dev -e esp32dev-qemu --qemu
```

### Configuration

Add QEMU environment to platformio.ini:

```ini
[env:esp32dev-qemu]
platform = https://github.com/pioarduino/platform-espressif32/releases/download/55.03.34/platform-espressif32.zip
board = esp32dev
framework = arduino
board_build.flash_mode = dio     # Required for QEMU
board_upload.flash_mode = dio    # Required for QEMU
```

### Requirements

- Docker installed and running
- `espressif/idf:latest` Docker image (pulled automatically)

### Known Limitations

1. **ESP32-S3 bootloader incompatibility**: The ESP32-S3 software bootloader contains QIO mode detection logic that crashes in QEMU. Use ESP32dev for QEMU testing instead.

2. **ESP32C6 chip ID mismatch**: QEMU doesn't have native ESP32C6 support yet. It falls back to ESP32C3 emulation, which causes chip ID validation failures.

3. **Performance**: QEMU emulation is slower than real hardware. Use for basic functional testing, not performance validation.

4. **Peripheral emulation**: Not all peripherals are fully emulated. Test on real hardware for production validation.

## Key Features

- **URL-based Package Management**: Direct URLs to toolchains and platforms - no hidden registries
- **Library Management**: Download and compile Arduino libraries from GitHub URLs
- **Fast Incremental Builds**: 0.76s rebuilds, 3s full builds (cached)
- **LTO Support**: Link-Time Optimization for optimal code size
- **Transparent Architecture**: Know exactly what's happening at every step
- **Real Downloads, No Mocks**: All packages are real, validated, and checksummed
- **Cross-platform Support**: Windows, macOS, and Linux
- **Modern Python**: 100% type-safe, PEP 8 compliant, tested

## Installation

```bash
# Install from PyPI (when published)
pip install fbuild

# Or install from source
git clone https://github.com/yourusername/fbuild.git
cd fbuild
pip install -e .
```

## Quick Start

### Building an Arduino Uno Project

1. **Create project structure**:
```bash
mkdir my-project && cd my-project
mkdir src
```

2. **Create platformio.ini**:
```ini
[env:uno]
platform = atmelavr
board = uno
framework = arduino
```

3. **Write your sketch** (`src/main.ino`):
```cpp
void setup() {
  pinMode(LED_BUILTIN, OUTPUT);
}

void loop() {
  digitalWrite(LED_BUILTIN, HIGH);
  delay(1000);
  digitalWrite(LED_BUILTIN, LOW);
  delay(1000);
}
```

4. **Build**:
```bash
fbuild build
```

On first build, Fbuild will:
- Download AVR-GCC toolchain (50MB, one-time)
- Download Arduino AVR core (5MB, one-time)
- Compile your sketch
- Generate `firmware.hex` in `.fbuild/build/uno/`

**Build time**: ~19s first build, ~3s subsequent builds, <1s incremental

## CLI Usage

### Build Command

```bash
# Build with auto-detected environment
fbuild build

# Build specific environment
fbuild build --environment uno
fbuild build -e mega

# Clean build (remove all build artifacts)
fbuild build --clean

# Verbose output (shows all compiler commands)
fbuild build --verbose

# Build in different directory
fbuild build --project-dir /path/to/project
```

### Output

```
Building environment: uno
Downloading toolchain: avr-gcc 7.3.0-atmel3.6.1-arduino7
Downloading: 100% ████████████████████ 50.1MB/50.1MB
Extracting package...
Toolchain ready at: .fbuild/cache/...
Downloading Arduino core: 1.8.6
Compiling sketch...
Compiling Arduino core...
Linking firmware...
Converting to Intel HEX...

✓ Build successful!

Firmware: .fbuild/build/uno/firmware.hex
Program: 1058 bytes (3.3% of 32256 bytes)
RAM: 9 bytes (0.4% of 2048 bytes)
Build time: 3.06s
```

## Configuration

### platformio.ini Reference

**Minimal configuration**:
```ini
[env:uno]
platform = atmelavr
board = uno
framework = arduino
```

**Full configuration**:
```ini
[platformio]
default_envs = uno

[env:uno]
platform = atmelavr
board = uno
framework = arduino
upload_port = COM3        # Future: for uploading
monitor_speed = 9600      # Future: for serial monitor
build_flags =
    -DDEBUG
    -DLED_PIN=13
lib_deps =
    https://github.com/FastLED/FastLED
    https://github.com/adafruit/Adafruit_NeoPixel
```

### Library Dependencies

Fbuild supports downloading and compiling Arduino libraries directly from GitHub URLs:

```ini
[env:uno]
platform = atmelavr
board = uno
framework = arduino
lib_deps =
    https://github.com/FastLED/FastLED
```

**Features**:
- Automatic GitHub URL optimization (converts repo URLs to zip downloads)
- Automatic branch detection (main vs master)
- Proper Arduino library structure handling
- LTO (Link-Time Optimization) for optimal code size
- Support for complex libraries with assembly optimizations

**Example build with FastLED**:
```
✓ Build successful!
Firmware: tests/platform/uno/.fbuild/build/uno/firmware.hex
Size: 12KB (4318 bytes program, 3689 bytes RAM)
Build time: 78.59 seconds
```


### Supported Platforms and Boards

**Arduino AVR Platform** - Fully Supported ✓
- **Arduino Uno** (atmega328p, 16MHz) - Fully tested ✓

**ESP32 Platform** - Supported ✓
- **ESP32 Dev** (esp32dev) - Supported ✓
- **ESP32-C3** (esp32-c3-devkitm-1) - Supported ✓
- **ESP32-C6** (esp32c6-devkit) - Supported ✓
- **ESP32-S3** (esp32-s3-devkitc-1) - Supported ✓
- **ESP32-S2** - Supported ✓
- **ESP32-H2** - Supported ✓
- **ESP32-P4** - Supported ✓
- **ESP32-C2** - Supported ✓ (v0.1.0+)
  - Uses skeleton library approach with ROM linker scripts
  - Full Arduino framework support
  - 220KB firmware size typical

**WASM / WebAssembly Platform** - Supported ✓
- Compiles Arduino/FastLED sketches to WebAssembly via Emscripten (`clang-tool-chain-emcc`)
- Outputs `firmware.js` + `firmware.wasm`
- Library dependencies from `lib_deps` compiled and linked automatically

**Planned Support**:
- Arduino Mega
- Arduino Nano
- Arduino Leonardo
- More AVR boards
## Performance

**Benchmarks** (Arduino Uno Blink sketch):

| Build Type | Time | Description |
|------------|------|-------------|
| First build | 19.25s | Includes toolchain download (50MB) |
| Full build | 3.06s | All packages cached |
| Incremental | 0.76s | No source changes |
| Clean build | 2.58s | Rebuild from cache |

**Firmware Size** (Blink):
- Program: 1,058 bytes (3.3% of 32KB flash)
- RAM: 9 bytes (0.4% of 2KB RAM)

## Key Benefits

### Transparency
Direct URLs and hash-based caching mean you know exactly what you're downloading. No hidden package registries or opaque dependency resolution.

### Reliability
Real downloads with checksum verification ensure consistent, reproducible builds. No mocks in production code.

### Speed
Optimized incremental builds complete in under 1 second, with intelligent caching for full rebuilds in 2-5 seconds.

### Code Quality
100% type-safe (mypy), PEP 8 compliant, and comprehensive test coverage ensure a maintainable and reliable codebase.

### Clear Error Messages
Actionable error messages with suggestions help you quickly identify and fix issues without requiring forum searches.

## Architecture

See [docs/build-system.md](docs/build-system.md) for comprehensive architecture documentation.
See [docs/architecture.dot](docs/architecture.dot) for a Graphviz diagram (render with `dot -Tpng`).

```
┌─────────────────────────────────────────────────────────────────────────────────────────┐
│                                      CLI LAYER                                          │
│  ┌─────────────────────────────────────────────────────────────────────────────────┐    │
│  │  cli.py                                                                         │    │
│  │  ├── build command ──────┐                                                      │    │
│  │  ├── deploy command ─────┼──► daemon/client.py ──► IPC (file-based) ──┐        │    │
│  │  └── monitor command ────┘                                             │        │    │
│  └────────────────────────────────────────────────────────────────────────│────────┘    │
└───────────────────────────────────────────────────────────────────────────│─────────────┘
                                                                            ▼
┌─────────────────────────────────────────────────────────────────────────────────────────┐
│                              DAEMON LAYER (Background Process)                          │
│                                                                                         │
│  ┌──────────────────────────────────────────────────────────────────────────────────┐  │
│  │  daemon.py (Server)                                                               │  │
│  │  ├── lock_manager.py (ResourceLockManager) ◄── Memory-based locks only!          │  │
│  │  ├── status_manager.py                                                            │  │
│  │  ├── process_tracker.py                                                           │  │
│  │  └── device_manager.py ──► device_discovery.py                                    │  │
│  │                         └── shared_serial.py                                      │  │
│  └──────────────────────────────────────────────────────────────────────────────────┘  │
│                            │                                                            │
│                            ▼                                                            │
│  ┌─────────────────────────────────────────────────────────────────────────────────┐   │
│  │  Request Processors (daemon/processors/)                                        │   │
│  │  ├── build_processor.py ────────────► BUILD LAYER                               │   │
│  │  ├── deploy_processor.py ───────────► DEPLOY LAYER                              │   │
│  │  ├── monitor_processor.py ──────────► monitor.py                                │   │
│  │  └── install_deps_processor.py ─────► PACKAGES LAYER                            │   │
│  └─────────────────────────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────────────────────────┘
                            │
         ┌──────────────────┼──────────────────┐
         ▼                  ▼                  ▼
┌─────────────────┐ ┌─────────────────┐ ┌─────────────────────────────────────────────────┐
│  CONFIG LAYER   │ │  PACKAGES LAYER │ │                  BUILD LAYER                    │
│                 │ │                 │ │                                                 │
│ ┌─────────────┐ │ │ ┌─────────────┐ │ │ ┌─────────────────────────────────────────────┐ │
│ │ ini_parser  │ │ │ │   cache.py  │ │ │ │  Platform Orchestrators                     │ │
│ │    .py      │ │ │ │      │      │ │ │ │  orchestrator.py (IBuildOrchestrator)       │ │
│ │ (PlatformIO │ │ │ │      ▼      │ │ │ │       │                                     │ │
│ │   Config)   │ │ │ │ downloader  │ │ │ │       ├── orchestrator_avr.py               │ │
│ │      │      │ │ │ │    .py      │ │ │ │       ├── orchestrator_esp32.py             │ │
│ │      ▼      │ │ │ └──────┬──────┘ │ │ │       ├── orchestrator_rp2040.py            │ │
│ │board_config │ │ │        │        │ │ │       ├── orchestrator_stm32.py             │ │
│ │    .py      │ │ │        ▼        │ │ │       └── orchestrator_teensy.py            │ │
│ │      │      │ │ │ ┌─────────────┐ │ │ └─────────────────────────────────────────────┘ │
│ │      ▼      │ │ │ │ Toolchains  │ │ │                      │                         │
│ │board_loader │ │ │ │ toolchain   │ │ │                      ▼                         │
│ │    .py      │ │ │ │   .py (AVR) │ │ │ ┌─────────────────────────────────────────────┐ │
│ │      │      │ │ │ │ toolchain_  │ │ │ │  Compilation                                │ │
│ │      ▼      │ │ │ │   esp32.py  │ │ │ │  source_scanner.py ──► compiler.py          │ │
│ │ mcu_specs   │ │ │ │ toolchain_  │ │ │ │                              │              │ │
│ │    .py      │ │ │ │   rp2040.py │ │ │ │  configurable_compiler.py    │              │ │
│ └─────────────┘ │ │ │     ...     │ │ │ │         │                    │              │ │
│                 │ │ └─────────────┘ │ │ │         ▼                    ▼              │ │
│                 │ │        │        │ │ │  flag_builder.py ──► compilation_executor   │ │
│                 │ │        ▼        │ │ └─────────────────────────────────────────────┘ │
│                 │ │ ┌─────────────┐ │ │                      │                         │
│                 │ │ │ Frameworks  │ │ │                      ▼                         │
│                 │ │ │arduino_core │ │ │ ┌─────────────────────────────────────────────┐ │
│                 │ │ │    .py      │ │ │ │  Linking                                    │ │
│                 │ │ │ framework_  │ │ │ │  linker.py ──► archive_creator.py           │ │
│                 │ │ │   esp32.py  │ │ │ │       │                                     │ │
│                 │ │ │     ...     │ │ │ │       ▼                                     │ │
│                 │ │ └─────────────┘ │ │ │  configurable_linker.py                     │ │
│                 │ │        │        │ │ │       │                                     │ │
│                 │ │        ▼        │ │ │       ▼                                     │ │
│                 │ │ ┌─────────────┐ │ │ │  binary_generator.py ──► firmware.hex/.bin  │ │
│                 │ │ │  Libraries  │ │ │ └─────────────────────────────────────────────┘ │
│                 │ │ │ library_    │ │ │                                                 │
│                 │ │ │  manager.py │ │ │  build_state.py (BuildStateTracker)             │
│                 │ │ │      │      │ │ └─────────────────────────────────────────────────┘
│                 │ │ │      ▼      │ │
│                 │ │ │ library_    │ │
│                 │ │ │ compiler.py │ │
│                 │ │ │      │      │ │
│                 │ │ │      ▼      │ │
│                 │ │ │github_utils │ │
│                 │ │ │platformio_  │ │
│                 │ │ │ registry.py │ │
│                 │ │ └─────────────┘ │
└─────────────────┘ └─────────────────┘

┌─────────────────────────────────────────────────────────────────────────────────────────┐
│                                    DEPLOY LAYER                                         │
│                                                                                         │
│  ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────┐    │
│  │  deployer.py    │  │ deployer_esp32  │  │   monitor.py    │  │  qemu_runner.py │    │
│  │  (IDeployer)    │  │    .py          │  │ (Serial Monitor)│  │   (Emulator)    │    │
│  │       │         │  │  (esptool)      │  │                 │  │                 │    │
│  │       ▼         │  └────────┬────────┘  └────────┬────────┘  └────────┬────────┘    │
│  │   [avrdude]     │           │                    │                    │             │
│  └────────┬────────┘           │                    │                    │             │
│           └────────────────────┴────────────────────┴────────────────────┘             │
│                                         │                                               │
│                                         ▼                                               │
│                          ┌──────────────────────────────┐                               │
│                          │     External Dependencies    │                               │
│                          │  esptool, avrdude, pyserial  │                               │
│                          │         Docker (QEMU)        │                               │
│                          └──────────────────────────────┘                               │
└─────────────────────────────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────────────────────────┐
│                                    LEDGER LAYER                                         │
│  ┌─────────────────────────────────┐  ┌─────────────────────────────────┐              │
│  │  ledger/board_ledger.py         │  │  daemon/firmware_ledger.py      │              │
│  │  (Board tracking)               │  │  (Firmware tracking)            │              │
│  └─────────────────────────────────┘  └─────────────────────────────────┘              │
└─────────────────────────────────────────────────────────────────────────────────────────┘
```

**Key Data Flows:**

1. **Build Request**: CLI → Daemon Client → Daemon Server → Build Processor → Platform Orchestrator → Compiler → Linker → firmware.hex/.bin
2. **Deploy Request**: CLI → Daemon Client → Daemon Server → Deploy Processor → Deployer (esptool/avrdude) → Device
3. **Package Download**: Orchestrator → Cache → Downloader → fingerprint verification → extracted packages

### Library System Architecture

The library management system handles downloading, compiling, and linking Arduino libraries:

1. **Library Downloading**
   - Optimizes GitHub URLs to direct zip downloads
   - Detects and uses appropriate branch (main/master)
   - Extracts libraries with proper directory structure

2. **Library Compilation**
   - Compiles C/C++ library sources with LTO flags (`-flto -fno-fat-lto-objects`)
   - Resolves include paths for Arduino library structure
   - Generates LTO bytecode objects for optimal linking

3. **Library Linking**
   - Passes library object files directly to linker (no archiving)
   - LTO-aware linking with `--allow-multiple-definition` for symbol resolution
   - Proper handling of weak symbols and ISR handlers

**Technical Solutions**:
- **LTO Bytecode**: Generate only LTO bytecode to avoid AVR register limitations during compilation
- **Direct Object Linking**: Pass object files directly to linker instead of archiving for better LTO integration
- **Multiple Definition Handling**: Support libraries that define symbols in multiple files (e.g., FastLED ISR handlers)

## Project Structure

```
my-project/
├── platformio.ini       # Configuration file
├── src/
│   ├── main.ino        # Your Arduino sketch
│   └── helpers.cpp     # Additional C++ files
└── .fbuild/               # Build artifacts (auto-generated)
    ├── cache/
    │   ├── packages/   # Downloaded toolchains
    │   └── extracted/  # Arduino cores
    └── build/
        └── uno/
            ├── src/          # Compiled sketch objects
            ├── core/         # Compiled Arduino core
            └── firmware.hex  # Final output ← Upload this!
```

## Testing

Fbuild includes comprehensive integration tests:

```bash
# Run all tests
pytest tests/

# Run integration tests only
pytest tests/integration/

# Run with verbose output
pytest -v tests/integration/

# Test results: 11/11 passing
```

**Test Coverage**:
- Full build success path
- Incremental builds
- Clean builds
- Firmware size validation
- HEX format validation
- Error handling (missing config, syntax errors, etc.)

## Troubleshooting

### Build fails with "platformio.ini not found"

Make sure you're in the project directory or use `-d`:
```bash
fbuild build -d /path/to/project
```

### Build fails with checksum mismatch

Clear cache and rebuild:
```bash
rm -rf .fbuild/cache/
fbuild build
```

### Compiler errors in sketch

Check the error message for line numbers:
```
Error: src/main.ino:5:1: error: expected ';' before '}' token
```

Common issues:
- Missing semicolon
- Missing closing brace
- Undefined function (missing #include or prototype)

### Slow builds

- First build with downloads: 15-30s (expected)
- Cached builds: 2-5s (expected)
- Incremental: <1s (expected)

If slower, check:
- Network speed (for downloads)
- Disk speed (SSD recommended)
- Use `--verbose` to see what's slow

See [docs/build-system.md](docs/build-system.md) for more troubleshooting.

## Development

To develop Fbuild, run `. ./activate.sh`

### Windows

This environment requires you to use `git-bash`.

### Linting

Run `./lint.sh` to find linting errors using `pylint`, `flake8` and `mypy`.

# Why

Both the Arduino CLI and PlatformIO build chains have lagged in their development. While PlatformIO represented a meaningful improvement over the original Arduino build system, it continues to exhibit significant architectural and reliability issues that have remained unresolved despite repeated community reports.

One persistent problem is PlatformIO’s tendency to corrupt its own global installation state. In practice, this often requires users to manually delete `~/.platformio/packages` to restore functionality. This behavior is particularly harmful to new developers, as the failure mode is opaque and recovery is undocumented. In addition, PlatformIO’s package management is frequently slow and unreliable: large toolchains for modern targets (e.g., ESP, STM, Raspberry Pi-class boards) are regularly invalidated and re-downloaded in full, sometimes consuming multiple gigabytes of bandwidth. Even trivial changes to `platformio.ini`—including non-functional edits such as comments—can trigger a full revalidation and reinstall cycle, especially when using the VS Code extension with autosave enabled. This makes the development experience unpredictable and fragile.

More critically, both Arduino CLI and PlatformIO fail to reliably enable essential compiler and linker features for embedded systems, most notably `--gc-sections` and link-time optimization (LTO). These features are fundamental for producing minimal binaries on memory-constrained devices, as they allow dead code elimination and cross–translation unit optimization. Their absence leads to substantial binary bloat. For FastLED, this limitation persisted for years and forced the project to rely on aggressive inlining strategies as a workaround—an approach that increases compile times and code complexity while still falling short of what proper LTO provides.

Compounding these technical issues, conflicts between PlatformIO and Espressif resulted in incomplete or delayed support for newer ESP targets. Boards such as the ESP32-C2, C5, and C6 required external workaround repositories to function correctly with the IDF v5 toolchain, despite being officially supported by the vendor. This further increased maintenance burden and slowed development.

Collectively, these issues cost the FastLED project months of developer time. PlatformIO serves as FastLED’s testing infrastructure, and repeated build failures, slow installs, and corrupted environments significantly reduced iteration speed. To enable concurrent builds and isolate failures, FastLED ultimately resorted to encapsulating the entire build chain inside Docker containers—solely to sandbox PlatformIO’s global state and avoid cross-contamination between builds.

FastLED attempted to address these shortcomings through feature requests and pull requests to PlatformIO; all were declined. Meanwhile, emerging low-cost, high-capability platforms—such as CH-series RISC-V microcontrollers, which are cheaper and more powerful than legacy ATtiny-class devices—remain effectively inaccessible under these legacy build systems.

Given this landscape, the cost to FastLED developers became untenable. It proved more efficient to rebuild the entire compile and deployment stack from first principles. The result is **fbuild**, the FastLED build system. With fbuild, builds are deterministic, fast, and scalable, and advanced compiler and linker features such as LTO can finally be enabled—both for modern targets and retroactively for legacy platforms where the tooling has supported them in theory for over a decade but failed to use them in practice.


## License

In the spirit of Dan Garcia's permissively licensed software, `fbuild` is presented as free software.

BSD 3-Clause License
