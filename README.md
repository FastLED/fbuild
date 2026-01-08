# Zapio

A modern, reliable replacement for PlatformIO that fixes all the bugs and simplifies embedded development.

[![Linting](../../actions/workflows/lint.yml/badge.svg)](../../actions/workflows/lint.yml)

[![MacOS_Tests](../../actions/workflows/push_macos.yml/badge.svg)](../../actions/workflows/push_macos.yml)
[![Ubuntu_Tests](../../actions/workflows/push_ubuntu.yml/badge.svg)](../../actions/workflows/push_ubuntu.yml)
[![Win_Tests](../../actions/workflows/push_win.yml/badge.svg)](../../actions/workflows/push_win.yml)

## What is Zapio?

Zapio is a next-generation embedded development tool designed to replace PlatformIO with a cleaner, more reliable architecture. Built from the ground up to address the pain points developers face with existing tools.

**Current Status**: v0.1.0 - Full Arduino Uno support with working build system

## Examples

`uv run zap deploy tests/esp32c6 --clean`
`uv run zap monitor --timeout 60 --halt-on-error "TEST FAILED" --halt-on-success "TEST PASSED"`

  * this will require the use of pyserial to attach to the usb
  * see how this is done in platformio


`uv run zap deploy tests/esp32c6 --no-build`
`uv run zap deploy tests/esp32c6/**/firmware.bin`

**complex**

`uv run zap deploy tests/esp32c6 --monitor="--timeout 60 --halt-on-error \"TEST FAILED\" --halt-on-success \"TEST PASSED\"`

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
pip install zapio

# Or install from source
git clone https://github.com/yourusername/zapio.git
cd zapio
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
zap build
```

On first build, Zapio will:
- Download AVR-GCC toolchain (50MB, one-time)
- Download Arduino AVR core (5MB, one-time)
- Compile your sketch
- Generate `firmware.hex` in `.zap/build/uno/`

**Build time**: ~19s first build, ~3s subsequent builds, <1s incremental

## CLI Usage

### Build Command

```bash
# Build with auto-detected environment
zap build

# Build specific environment
zap build --environment uno
zap build -e mega

# Clean build (remove all build artifacts)
zap build --clean

# Verbose output (shows all compiler commands)
zap build --verbose

# Build in different directory
zap build --project-dir /path/to/project
```

### Output

```
Building environment: uno
Downloading toolchain: avr-gcc 7.3.0-atmel3.6.1-arduino7
Downloading: 100% ████████████████████ 50.1MB/50.1MB
Extracting package...
Toolchain ready at: .zap/cache/...
Downloading Arduino core: 1.8.6
Compiling sketch...
Compiling Arduino core...
Linking firmware...
Converting to Intel HEX...

✓ Build successful!

Firmware: .zap/build/uno/firmware.hex
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

Zapio supports downloading and compiling Arduino libraries directly from GitHub URLs:

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
Firmware: tests/uno/.zap/build/uno/firmware.hex
Size: 12KB (4318 bytes program, 3689 bytes RAM)
Build time: 78.59 seconds
```

### Supported Boards

Currently supported:
- **Arduino Uno** (atmega328p, 16MHz) - Fully tested ✓

Planned support:
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

## Why Zapio over PlatformIO?

### Transparency
- **Zapio**: Direct URLs, hash-based caching, know exactly what you're downloading
- **PlatformIO**: Hidden package registry, opaque dependency resolution

### Reliability
- **Zapio**: Real downloads with checksum verification, no mocks in production
- **PlatformIO**: Complex dependency chains, frequent breaking changes

### Speed
- **Zapio**: Optimized incremental builds (<1s), smart caching
- **PlatformIO**: Can be slow, especially with clean builds

### Code Quality
- **Zapio**: 100% type-safe (mypy), PEP 8 compliant, comprehensive tests
- **PlatformIO**: Mixed code quality, harder to contribute to

### Error Messages
- **Zapio**: Clear, actionable error messages with suggestions
- **PlatformIO**: Often cryptic errors requiring forum searches

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
└── .zap/               # Build artifacts (auto-generated)
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

Zapio includes comprehensive integration tests:

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
zap build -d /path/to/project
```

### Build fails with checksum mismatch

Clear cache and rebuild:
```bash
rm -rf .zap/cache/
zap build
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

To develop Zapio, run `. ./activate.sh`

### Windows

This environment requires you to use `git-bash`.

### Linting

Run `./lint.sh` to find linting errors using `pylint`, `flake8` and `mypy`.

## License

BSD 3-Clause License
