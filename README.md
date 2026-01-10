# Fbuild

A fast, next-generation multi-platform compiler, deployer, and monitor for embedded development.

[![Linting](../../actions/workflows/lint.yml/badge.svg)](../../actions/workflows/lint.yml)

[![MacOS_Tests](../../actions/workflows/push_macos.yml/badge.svg)](../../actions/workflows/push_macos.yml)
[![Ubuntu_Tests](../../actions/workflows/push_ubuntu.yml/badge.svg)](../../actions/workflows/push_ubuntu.yml)
[![Win_Tests](../../actions/workflows/push_win.yml/badge.svg)](../../actions/workflows/push_win.yml)

## What is Fbuild?

Fbuild is a next-generation embedded development tool featuring a clean, transparent architecture. It provides fast incremental builds, URL-based package management, and comprehensive multi-platform support for Arduino and ESP32 development.

**Current Status**: v0.1.0 - Full Arduino Uno support with working build system

## Examples

**Quick start** - Build, deploy, and monitor in one command:

```bash
# Default action: build + deploy + monitor
uv run fbuild tests/esp32c6
```

**Deploy commands:**

```bash
# Deploy with clean build
uv run fbuild deploy tests/esp32c6 --clean

# Deploy with monitoring and test patterns
uv run fbuild deploy tests/esp32c6 --monitor="--timeout 60 --halt-on-error \"TEST FAILED\" --halt-on-success \"TEST PASSED\""
```

**Monitor command:**

```bash
# Monitor serial output with pattern matching
uv run fbuild monitor --timeout 60 --halt-on-error "TEST FAILED" --halt-on-success "TEST PASSED"
```

  * Serial monitoring requires pyserial to attach to the USB device
  * Port auto-detection works similarly to PlatformIO

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
Firmware: tests/uno/.fbuild/build/uno/firmware.hex
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

**Planned Support**:
- Arduino Mega
- Arduino Nano
- Arduino Leonardo
- More AVR boards
- Teensy 3.x/4.x platforms
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

**High-level flow**:
```
platformio.ini → Config Parser → Package Manager → Build Orchestrator
                                        ↓
                      ┌─────────────────┼─────────────────┬──────────────────┐
                      ↓                 ↓                 ↓                  ↓
                 Toolchain        Arduino Core      Source Scanner   Library Manager
                (AVR-GCC)       (cores/variants)     (.ino/.cpp/.c)   (GitHub libs)
                      ↓                 ↓                 ↓                  ↓
                      └─────────────────┼─────────────────┴──────────────────┘
                                        ↓
                              Compiler (avr-g++)
                                        ↓
                              Linker (avr-gcc)
                                        ↓
                         objcopy (ELF → HEX)
                                        ↓
                                firmware.hex
```

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

## License

BSD 3-Clause License
