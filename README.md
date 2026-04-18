![fbuild](https://github.com/user-attachments/assets/7db78eba-b10f-44c7-ae32-7fc0b5e46642)

*A fast, next-generation multi-platform compiler, deployer, and monitor for embedded development, directly compatible with `platformio.ini`.*

## CI status

[![Check Ubuntu](https://github.com/fastled/fbuild/actions/workflows/check-ubuntu.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/check-ubuntu.yml)
[![Check macOS](https://github.com/fastled/fbuild/actions/workflows/check-macos.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/check-macos.yml)
[![Check Windows](https://github.com/fastled/fbuild/actions/workflows/check-windows.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/check-windows.yml)
[![Formatting](https://github.com/fastled/fbuild/actions/workflows/fmt.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/fmt.yml)
[![Min Rust Version](https://github.com/fastled/fbuild/actions/workflows/msrv.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/msrv.yml)
[![Documentation](https://github.com/fastled/fbuild/actions/workflows/docs.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/docs.yml)
[![Validate Boards](https://github.com/fastled/fbuild/actions/workflows/validate-boards.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/validate-boards.yml)
[![Build Native Binaries](https://github.com/fastled/fbuild/actions/workflows/build.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build.yml)

Per-platform board build badges live in [`docs/BOARD_STATUS.md`](docs/BOARD_STATUS.md).

# fbuild

`fbuild` is a next-generation embedded development tool featuring a clean, extensible, data-driven architecture. It provides fast incremental builds, URL-based package management, and comprehensive multi-platform support for Arduino and ESP32 development.

**`platformio.ini` compatible** — fbuild uses the same `platformio.ini` already used in your PlatformIO sketches.

**Current status**: v2.0.6 — Rust rewrite. Full ESP32 / Teensy support with a working build system.

## Docs index

For a full FAQ-style map of every doc in this repo, see [`docs/INDEX.md`](docs/INDEX.md). Common entry points:

- **Why fbuild exists, key benefits, performance** → [`docs/WHY.md`](docs/WHY.md)
- **Is my board supported?** → [`docs/BOARD_STATUS.md`](docs/BOARD_STATUS.md)
- **Architecture / daemon / serial internals** → [`docs/architecture/`](docs/architecture/) (start at [`overview.md`](docs/architecture/overview.md))
- **Crate dependency graph** → [`crates/CLAUDE.md`](crates/CLAUDE.md)
- **Testing, troubleshooting, local setup** → [`docs/DEVELOPMENT.md`](docs/DEVELOPMENT.md)
- **Design decisions (ADRs)** → [`docs/DESIGN_DECISIONS.md`](docs/DESIGN_DECISIONS.md)
- **Implementation roadmap** → [`docs/ROADMAP.md`](docs/ROADMAP.md)

## Installation

```bash
# Install from PyPI
pip install fbuild

# Or install from source
git clone https://github.com/fastled/fbuild.git
cd fbuild
pip install -e .
```

## Quick Start

### Build an Arduino Uno project

1. **Create project structure**:
   ```bash
   mkdir my-project && cd my-project
   mkdir src
   ```

2. **Create `platformio.ini`**:
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

On first build, fbuild will download the AVR-GCC toolchain (~50MB, one-time), download the Arduino AVR core (~5MB, one-time), compile your sketch, and emit `firmware.hex` under `.fbuild/build/uno/`.

**Build time**: ~19s first build, ~3s subsequent builds, <1s incremental.

## Examples

Build, deploy, and monitor in one command:

```bash
# Default action: build + deploy
fbuild tests/platform/esp32c6

# Default action: build + deploy + monitor
fbuild tests/platform/esp32c6 --monitor
```

**Deploy commands**:

```bash
# Deploy with clean build
fbuild deploy tests/platform/esp32c6 --clean

# Deploy with monitoring and test patterns
fbuild deploy tests/platform/esp32c6 \
  --monitor="--timeout 60 --halt-on-error \"TEST FAILED\" --halt-on-success \"TEST PASSED\""

# Deploy to the default emulator backend
fbuild deploy tests/platform/uno -e uno --to emu

# Deploy to the default emulator backend and open the monitor page
fbuild deploy tests/platform/uno -e uno --to emu --monitor

# Deploy ESP32-S3 to the default emulator backend
fbuild deploy tests/platform/esp32s3 -e esp32s3 --to emu --monitor --timeout 10 --verbose

# Deploy to an explicit emulator backend
fbuild deploy tests/platform/esp32s3 -e esp32s3 --to emu --emulator qemu --monitor --timeout 10
```

**`test-emu`** — build + run in emulator in one step (CI-friendly, exits with emulator exit code):

```bash
# Auto-detect emulator backend from the board
fbuild test-emu tests/platform/uno -e uno

# Explicit backend, with pattern matching
fbuild test-emu tests/platform/esp32s3 -e esp32s3 --emulator qemu --timeout 10

# AVR with simavr backend
fbuild test-emu tests/platform/mega -e megaatmega2560 --emulator simavr

# Halt on first test result
fbuild test-emu tests/platform/uno -e uno \
  --halt-on-success "TEST PASSED" --halt-on-error "TEST FAILED"
```

| Option | Description |
|--------|-------------|
| `--emulator <backend>` | Force backend: `avr8js`, `qemu`, or `simavr` (auto-detected if omitted) |
| `--timeout <secs>` | Kill the emulator after N seconds |
| `--halt-on-success <regex>` | Stop and report pass when pattern matches |
| `--halt-on-error <regex>` | Stop and report fail when pattern matches |
| `--expect <regex>` | Require this pattern in output (fail on timeout if missing) |
| `--no-timestamp` | Disable timestamp prefix on output lines |
| `-v, --verbose` | Show emulator command and build details |

**Monitor command**:

```bash
# Monitor serial output with pattern matching
fbuild monitor --timeout 60 --halt-on-error "TEST FAILED" --halt-on-success "TEST PASSED"
```

Serial monitoring requires pyserial to attach to the USB device. Port auto-detection works similarly to PlatformIO.

## Emulator testing

fbuild can build and run firmware in emulators without physical hardware. Two entry points:

- **`fbuild test-emu`** — one-shot build + emulate + exit (CI-friendly)
- **`fbuild deploy --to emu`** — deploy flow with optional `--monitor`

Both auto-detect the emulator backend from the board, or accept `--emulator <backend>`.

### Emulator backends

| Backend | Platforms | MCUs | Requirements |
|---------|-----------|------|--------------|
| **avr8js** | AtmelAVR | ATmega328P | Node.js (bundled headless script) |
| **simavr** | AtmelAVR, MegaAVR | ATmega2560, ATmega32U4, and others | `simavr` binary on PATH |
| **qemu** | Espressif32 | ESP32, ESP32-S3 (Xtensa); ESP32-C3, ESP32-C6, ESP32-H2 (RISC-V) | Native QEMU (auto-downloaded) |

Auto-detection rules when `--emulator` is omitted:

- ATmega328P defaults to **avr8js** (no external binary needed)
- Other AVR MCUs with `simavr` in `debug_tools` default to **simavr**
- ESP32, ESP32-S3 (Xtensa) and ESP32-C3, ESP32-C6, ESP32-H2 (RISC-V) default to **qemu**

### QEMU notes

ESP32-family QEMU runs from a normal Arduino environment. fbuild launches the correct Espressif QEMU binary (`qemu-system-xtensa` for ESP32/ESP32-S3; `qemu-system-riscv32` for ESP32-C3/C6/H2) based on the selected `board`. The required QEMU build flags are injected automatically when deploying to `--to emu`.

```ini
[env:esp32s3]
platform = https://github.com/pioarduino/platform-espressif32/releases/download/55.03.34/platform-espressif32.zip
board = esp32-s3-devkitc-1
framework = arduino
board_build.flash_mode = dio
board_upload.flash_mode = dio

[env:esp32c3]
platform = https://github.com/pioarduino/platform-espressif32/releases/download/55.03.34/platform-espressif32.zip
board = esp32-c3-devkitm-1
framework = arduino
board_build.flash_mode = dio
board_upload.flash_mode = dio
```

QEMU requires DIO flash mode. Boards configured with `qio` or `qout` will fail fast before building.

QEMU runtime is native-only. Supported hosts: Linux x86_64/arm64, macOS x86_64/arm64, Windows x86_64. On Windows, fbuild stages the required QEMU runtime DLLs for the managed install.

**Known limitations**:

1. **ESP32 QEMU** supports ESP32, ESP32-S3 (Xtensa) and ESP32-C3, ESP32-C6, ESP32-H2 (RISC-V). ESP32-S2 and ESP32-P4 are not yet supported by upstream Espressif QEMU.
2. **QEMU-specific firmware patching**: fbuild patches the generated ESP32-S3 app image for QEMU to bypass an ADC calibration constructor that hangs under emulation, then repairs the image checksum and hash. RISC-V variants (C3/C6/H2) do not require this patch.
3. **Performance**: QEMU emulation is slower than real hardware. Use it for functional validation, not timing-sensitive behavior.
4. **Peripheral coverage**: Not all peripherals are fully emulated. Real hardware is still required for production validation.

## CLI Usage

### Build

```bash
# Build with auto-detected environment
fbuild build

# Build a specific environment
fbuild build --environment uno
fbuild build -e mega

# Clean build (remove all build artifacts)
fbuild build --clean

# Verbose output (shows all compiler commands)
fbuild build --verbose

# Build in a different directory
fbuild build --project-dir /path/to/project
```

Typical output:

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

### `platformio.ini` summary

**Minimal configuration**:

```ini
[env:uno]
platform = atmelavr
board = uno
framework = arduino
```

**Fuller configuration**:

```ini
[platformio]
default_envs = uno

[env:uno]
platform = atmelavr
board = uno
framework = arduino
upload_port = COM3
monitor_speed = 9600
build_flags =
    -DDEBUG
    -DLED_PIN=13
lib_deps =
    https://github.com/FastLED/FastLED
    https://github.com/adafruit/Adafruit_NeoPixel
```

### Library dependencies

fbuild supports downloading and compiling Arduino libraries directly from GitHub URLs:

```ini
[env:uno]
platform = atmelavr
board = uno
framework = arduino
lib_deps =
    https://github.com/FastLED/FastLED
```

Features:

- Automatic GitHub URL optimization (converts repo URLs to zip downloads)
- Automatic branch detection (main vs master)
- Proper Arduino library structure handling
- LTO (Link-Time Optimization) for optimal code size
- Support for complex libraries with assembly optimizations

## Key features

- **URL-based package management** — direct URLs to toolchains and platforms, no hidden registries
- **Library management** — download and compile Arduino libraries from GitHub URLs
- **Fast incremental builds** — 0.76s rebuilds, 3s full builds (cached)
- **LTO support** — Link-Time Optimization for optimal code size
- **Transparent architecture** — know exactly what's happening at every step
- **Real downloads, no mocks** — all packages are real, validated, and checksummed
- **Cross-platform** — Windows, macOS, and Linux
- **`platformio.ini` compatible** — drop-in replacement for PlatformIO builds

See [`docs/WHY.md`](docs/WHY.md) for the full rationale, benefits, and performance benchmarks.

## Supported platforms

fbuild supports AVR, MegaAVR, Renesas RA, ESP8266, ESP32 (all variants incl. S2/S3/C2/C3/C5/C6/H2/P4), CH32V/CH32X RISC-V, Teensy (LC–4.1), STM32, Atmel SAM/SAMD, RP2040/RP2350, Nordic NRF52, Apollo3, Silicon Labs EFR32, and WASM via Emscripten.

For the live per-platform CI badge matrix, per-board status, and family deep-dives, see [`docs/BOARD_STATUS.md`](docs/BOARD_STATUS.md).

## Project structure

The repo is a Rust workspace. For the full crate dependency graph and per-crate responsibilities, see [`crates/CLAUDE.md`](crates/CLAUDE.md).

A typical user project on disk looks like:

```
my-project/
├── platformio.ini       # Configuration file
├── src/
│   ├── main.ino         # Your Arduino sketch
│   └── helpers.cpp      # Additional C++ files
└── .fbuild/             # Build artifacts (auto-generated)
    ├── cache/
    │   ├── packages/    # Downloaded toolchains
    │   └── extracted/   # Arduino cores
    └── build/
        └── uno/
            ├── src/           # Compiled sketch objects
            ├── core/          # Compiled Arduino core
            └── firmware.hex   # Final output ← upload this
```

## Architecture

Architecture docs are decentralized under [`docs/architecture/`](docs/architecture/). Start with [`overview.md`](docs/architecture/overview.md), then read the subsystem-specific docs listed in [`docs/CLAUDE.md`](docs/CLAUDE.md).

## Development

Testing, troubleshooting, linting, and local setup instructions live in [`docs/DEVELOPMENT.md`](docs/DEVELOPMENT.md). Project-wide rules for contributors and LLM agents are in [`CLAUDE.md`](CLAUDE.md).

## License

In the spirit of Dan Garcia's permissively licensed software, `fbuild` is presented as free software.

BSD 3-Clause License
