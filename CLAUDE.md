# fbuild - Modern Embedded Development Tool

## Project Overview

fbuild is a next-generation embedded development tool designed to replace PlatformIO with a cleaner, more reliable architecture. It provides transparent URL-based package management, fast incremental builds, and comprehensive support for Arduino and ESP32 platforms.

**Current Version:** v1.2.9
**Status:** Full Arduino Uno support with working build system
**Language:** Python 3.10+ (Type-safe, PEP 8 compliant)

## Key Features

- Compiles Arduino sketches using native toolchains (AVR-GCC, ESP32 toolchains)
- Transparent URL-based package management (no hidden registries)
- Fast incremental builds (0.76s rebuilds, 3s full builds)
- Library dependency management from GitHub URLs and local paths (symlinks)
- **Environment inheritance** - Full support for `extends` directive in platformio.ini
- **Source directory override** - Configure custom source directories (e.g., examples/)
- **Board build customization** - Support for board_build.* and board_upload.* overrides
- Cross-platform support (Windows, macOS, Linux)
- 100% type-safe with comprehensive testing

## Project Structure

### Core Source Code (`src/fbuild/`)

#### CLI Interface
- **`cli.py`** - Main command-line interface with three commands:
  - `fbuild build` - Build firmware
  - `fbuild deploy` - Deploy firmware to device
  - `fbuild monitor` - Monitor serial output

#### Build System (`src/fbuild/build/`)
- **`orchestrator.py`** - Coordinates the entire build pipeline
- **`compiler.py`** - C/C++ compilation wrapper
- **`configurable_compiler.py`** - Configurable compiler module
- **`linker.py`** - Linking and firmware generation
- **`configurable_linker.py`** - Configurable linker module
- **`source_scanner.py`** - Source file discovery and .ino preprocessing

#### Configuration (`src/fbuild/config/`)
- **`ini_parser.py`** - PlatformIO.ini file parsing
- **`board_config.py`** - Board-specific configuration loading

#### Package Management (`src/fbuild/packages/`)
- **`downloader.py`** - HTTP download with checksum verification
- **`toolchain.py`** - AVR toolchain management
- **`arduino_core.py`** - Arduino core library management
- **`library_manager.py`** - GitHub library dependency management
- **`cache.py`** - Package caching system
- **`esp32_platform.py`** - ESP32 platform support
- **`esp32_toolchain.py`** - ESP32 toolchain management
- **`esp32_framework.py`** - ESP32 framework support
- **`platformio_registry.py`** - PlatformIO registry integration

#### Deployment (`src/fbuild/deploy/`)
- **`deployer.py`** - Firmware uploading (ESP32 support via esptool)
- **`monitor.py`** - Serial port monitoring with pattern matching

### Documentation (`docs/`)
- **`build-system.md`** - Detailed build architecture and component breakdown
- **`platformio-ini-spec.md`** - Configuration file specification
- **`arduino-core-structure.md`** - Arduino core organization
- **`toolchain-packages.md`** - Toolchain details and package information
- **`PLATFORM_CONFIG_FORMAT.md`** - Platform configuration format specification
- **`DEVELOPMENT.md`** - Development mode guide for contributors

### Tests (`tests/`)
Integration test projects for multiple platforms:
- **`tests/uno/`** - Arduino Uno test project
- **`tests/esp32c6/`** - ESP32-C6 test project
- **`tests/esp32c3/`** - ESP32-C3 test project
- **`tests/esp32s3/`** - ESP32-S3 test project
- **`tests/esp32dev/`** - ESP32 Dev test project

## Architecture

### Build Pipeline Flow
```
CLI Entry Point (cli.py)
    ↓
BuildOrchestrator (orchestrator.py)
    ↓
Config Parser (ini_parser.py) → Board Config (board_config.py)
    ↓
Package Manager (toolchain.py, arduino_core.py, library_manager.py)
    ↓
Source Scanner (source_scanner.py) - Discovers and preprocesses files
    ↓
Compiler (compiler.py) - Compiles source files to objects
    ↓
Linker (linker.py) - Links objects and generates firmware
    ↓
Output: firmware.hex / firmware.bin
```

### Key Components

1. **Configuration System** - Parses `platformio.ini` and loads board-specific settings
2. **Package Management** - Downloads, caches, and validates toolchains, cores, and libraries
3. **Source Scanner** - Discovers source files and preprocesses Arduino .ino files
4. **Compiler** - Wraps toolchain compilers (avr-gcc/xtensa-gcc) with proper flags
5. **Linker** - Links object files and converts to firmware format
6. **Library Manager** - Downloads and compiles GitHub library dependencies with LTO
7. **Build Orchestrator** - Coordinates all phases into a unified pipeline
8. **Deployer** - Handles firmware upload to devices
9. **Serial Monitor** - Monitors device output with pattern matching

## Technology Stack

### Core Dependencies
- **Python 3.10+** - Primary language
- **requests** - HTTP downloads
- **tqdm** - Progress bars
- **pyserial** - Serial communication
- **esptool** - ESP32 flashing

### Build Tools
- **AVR-GCC 7.3.0-atmel3.6.1-arduino7** - Arduino AVR compilation (auto-downloaded)
- **Arduino AVR Core** - Downloaded from GitHub
- **ESP32 Toolchains** - Platform-specific toolchains

### Development Tools
- **pytest** + **pytest-cov** - Testing and coverage
- **ruff** - Fast Python linter
- **pylint** - Code analysis
- **mypy** / **pyright** - Type checking
- **isort** - Import sorting

## Configuration

### Main Configuration File: `platformio.ini`
Standard PlatformIO format with environment sections:
```ini
[env:uno]
platform = atmelavr
board = uno
framework = arduino
build_flags = -DCUSTOM_FLAG
lib_deps =
    https://github.com/user/library
```

### Key Configuration Options
- **platform** - Target platform (atmelavr, espressif32)
- **board** - Target board (uno, esp32dev, esp32c6, etc.)
- **framework** - Framework to use (arduino)
- **build_flags** - Compiler flags
- **lib_deps** - Library dependencies (GitHub URLs, local paths, symlinks)
- **extends** - Inherit configuration from another environment
- **board_build.*** - Board-specific build settings (flash_mode, flash_size, partitions, etc.)
- **board_upload.*** - Board-specific upload settings
- **src_dir** (in [platformio] section) - Override source directory path
- **upload_port** - Serial port for uploading
- **monitor_speed** - Serial monitor baud rate

### Advanced Configuration Features

#### Environment Inheritance (`extends`)
Environments can inherit from other environments using the `extends` directive:

```ini
[env:generic-esp]
platform = espressif32
framework = arduino
build_flags = -DDEBUG

[env:esp32c6]
extends = env:generic-esp
board = esp32-c6-devkitc-1
build_flags = ${env:generic-esp.build_flags} -DSPECIFIC_FLAG
```

Features:
- Multi-level inheritance (child -> parent -> grandparent)
- Automatic circular dependency detection
- Variable substitution with `${env:parent.key}` syntax
- Abstract base environments (without required fields) are supported

#### Source Directory Override (`src_dir`)
Customize the source directory location in the [platformio] section:

```ini
[platformio]
src_dir = examples/Blink
default_envs = esp32c6

[env:esp32c6]
platform = espressif32
board = esp32-c6-devkitc-1
framework = arduino
```

This is particularly useful for building example sketches from subdirectories.

#### Board Build Customization
Override board-specific build settings:

```ini
[env:esp32c6]
platform = espressif32
board = esp32-c6-devkitc-1
framework = arduino
board_build.flash_mode = dio
board_build.flash_size = 4MB
board_build.partitions = huge_app.csv
board_upload.flash_size = 4MB
```

Common board_build options:
- `flash_mode` - Flash access mode (dio, qio, qout, dout)
- `flash_size` - Flash memory size (4MB, 8MB, 16MB, etc.)
- `partitions` - Partition table CSV file
- `mcu` - MCU type override
- `f_cpu` - CPU frequency override

#### Library Dependencies with Symlinks
Support for local library development using symlinks:

```ini
[env:esp32c6]
platform = espressif32
board = esp32-c6-devkitc-1
framework = arduino
lib_deps =
    FastLED=symlink://./
    https://github.com/user/library
```

On Windows, symlinks are automatically converted to directory copies for compatibility.

## CLI Usage

### Default Action (Quick Start)
```bash
# Build, deploy, and monitor in one command
fbuild [project_dir]

# Example:
fbuild tests/esp32c6
```
This is equivalent to `fbuild deploy [project_dir] --monitor`

### Build Command
```bash
fbuild build [project_dir] -e [environment] [-c/--clean] [-v/--verbose]
```

### Deploy Command
```bash
fbuild deploy [project_dir] -e [environment] [-p/--port] [-c/--clean] [--monitor]
```

### Monitor Command
```bash
fbuild monitor [project_dir] -e [environment] [-p/--port] [-b/--baud]
            [--halt-on-error] [--halt-on-success] [-t/--timeout]
```

## Development Workflow

### Setting Up Development Environment
1. Clone the repository
2. Install in development mode: `pip install -e .`
3. **Enable development mode:** `export FBUILD_DEV_MODE=1` (see DEVELOPMENT.md)
4. Run tests: `pytest`
5. Check types: `mypy src/fbuild`
6. Lint code: `ruff check src/fbuild`

### Development Mode
When developing fbuild itself, **always set `FBUILD_DEV_MODE=1`** to isolate:
- Daemon files in `.fbuild/daemon_dev/` (instead of `~/.fbuild/daemon/`)
- Cache files in `.fbuild/cache_dev/` (instead of `.fbuild/cache/`)

This prevents interference with production fbuild installations. See `DEVELOPMENT.md` for details.

### Running Tests
The `tests/` directory contains integration test projects for various platforms. Each test project has a `platformio.ini` configuration and example sketches.

### Common Development Tasks

**Build a test project:**
```bash
fbuild build tests/uno -e uno
```

**Deploy to device:**
```bash
fbuild deploy tests/esp32dev -e esp32dev --monitor
```

**Run with verbose output:**
```bash
fbuild build tests/uno -e uno -v
```

## Performance

- **Incremental builds:** ~0.76s
- **Full builds:** ~3s (Arduino Uno)
- **Package caching:** Automatic with checksum verification

## Supported Platforms

### Currently Supported
- **Arduino Uno** (atmelavr platform) - Full support
- **ESP32 variants** - In development:
  - ESP32 Dev
  - ESP32-C3
  - ESP32-C6
  - ESP32-S3

### Platform Support Status
- Arduino AVR: ✅ Complete
- ESP32: 🚧 In progress

## Recent Development Activity

Latest commits show active development:
- `feat(build): add configurable compiler and linker modules` (8318d54)
- `feat(library): add support for downloading and compiling library dependencies` (4939425)
- `update support for esp platforms` (0426c25)
- `fix(source_scanner): exclude more directories from scanning` (da0cf08)

## Troubleshooting

### Common Issues

**Build failures:**
- Check `platformio.ini` syntax
- Verify board configuration exists
- Check toolchain download status
- Use `-v` flag for verbose output

**Package download issues:**
- Check internet connection
- Clear package cache in `~/.fbuild/`
- Verify GitHub URLs in `lib_deps`

**Serial port access:**
- Ensure user has permission to access serial ports
- Check correct port with `--port` option
- Verify device is connected

## Version Bumping Protocol

When bumping the version, update **all three locations** to keep them in sync:

1. **`src/fbuild/__init__.py`** - The `__version__` variable (canonical source)
2. **`pyproject.toml`** - The `version` field under `[project]`
3. **`CLAUDE.md`** - The `**Current Version:**` line in Project Overview

Version format follows [Semantic Versioning](https://semver.org/): `MAJOR.MINOR.PATCH`
- **PATCH**: Bug fixes, minor improvements (e.g., 1.2.7 → 1.2.8)
- **MINOR**: New features, backward-compatible (e.g., 1.2.8 → 1.3.0)
- **MAJOR**: Breaking changes (e.g., 1.3.0 → 2.0.0)

## Contributing

This project follows:
- PEP 8 coding standards
- Type hints for all functions
- Comprehensive test coverage
- Clear documentation

## Coding Guidelines

### Locking Strategy: Memory-Based Daemon Locks Only

**IMPORTANT:** This project uses **only memory-based daemon locks** (held by the daemon process in memory via `threading.Lock`). File-based locks using `fcntl`, `msvcrt`, or `.lock` files are **NOT allowed**.

**Rationale:**
- Cross-process synchronization is handled by the daemon which holds locks in memory
- File-based locks are problematic on Windows and can cause issues with stale lock files
- The daemon provides a centralized point of coordination for all clients

**What this means for contributors:**
- Use `threading.Lock` for in-process synchronization within a single process
- Do NOT use `fcntl.flock()`, `msvcrt.locking()`, or similar file-based locking mechanisms
- Do NOT create `.lock` files for cross-process synchronization
- All cross-process coordination must go through the daemon's lock manager (`ResourceLockManager`)
- Ledger files (board_ledger.json, firmware_ledger.json) use `threading.Lock` internally; cross-process safety is guaranteed by the daemon

## Additional Resources

- **README.md** - Comprehensive user guide with quick start
- **docs/build-system.md** - Detailed architecture documentation
- **docs/platformio-ini-spec.md** - Configuration reference
- **tests/** - Example projects demonstrating usage
