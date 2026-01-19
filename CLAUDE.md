# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

fbuild is a PlatformIO-compatible embedded development tool providing build, deploy, and monitor functionality for Arduino/ESP32 platforms. It uses URL-based package management and a daemon for cross-process coordination.

**Current Version:** v1.2.11 (update in `src/fbuild/__init__.py`, `pyproject.toml`, and this file)

## Development Commands

```bash
# Install in development mode
pip install -e .

# IMPORTANT: Enable dev mode to isolate from production
export FBUILD_DEV_MODE=1  # Linux/macOS
set FBUILD_DEV_MODE=1     # Windows CMD

# Run unit tests (fast, parallel)
uv run --group test pytest -n auto tests/unit -v

# Run single test file
uv run --group test pytest tests/unit/test_foo.py -v

# Run all tests including integration (slow)
./test --full

# Lint and format
./lint  # Runs: ruff, black, isort, pyright, flake8

# Build a test project
fbuild build tests/uno -e uno
fbuild build tests/esp32c6 -e esp32c6 -v  # verbose

# Deploy and monitor
fbuild deploy tests/esp32c6 --monitor
```

## Architecture

See `docs/architecture.dot` for Graphviz diagram. Render with: `dot -Tpng docs/architecture.dot -o architecture.png`

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                               CLI LAYER                                     │
│  cli.py ──► build/deploy/monitor commands ──► daemon/client.py (IPC)       │
└─────────────────────────────────────────────────────────────────────────────┘
                                      │
                                      ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                      DAEMON LAYER (Background Process)                      │
│  daemon/daemon.py                                                           │
│  ├── lock_manager.py (ResourceLockManager) ◄── Memory-based locks only!    │
│  ├── device_manager.py ──► device_discovery.py, shared_serial.py           │
│  └── processors/                                                            │
│      ├── build_processor.py ────► BUILD LAYER                               │
│      ├── deploy_processor.py ───► DEPLOY LAYER                              │
│      └── monitor_processor.py ──► monitor.py                                │
└─────────────────────────────────────────────────────────────────────────────┘
           │                    │                    │
           ▼                    ▼                    ▼
┌─────────────────┐  ┌─────────────────┐  ┌───────────────────────────────────┐
│  CONFIG LAYER   │  │ PACKAGES LAYER  │  │          BUILD LAYER              │
│                 │  │                 │  │                                   │
│ ini_parser.py   │  │ cache.py        │  │ Platform Orchestrators:           │
│ (PlatformIO     │  │     │           │  │ orchestrator.py (interface)       │
│  Config)        │  │     ▼           │  │ ├── orchestrator_avr.py           │
│     │           │  │ downloader.py   │  │ ├── orchestrator_esp32.py         │
│     ▼           │  │     │           │  │ ├── orchestrator_rp2040.py        │
│ board_config.py │  │     ▼           │  │ ├── orchestrator_stm32.py         │
│     │           │  │ Toolchains:     │  │ └── orchestrator_teensy.py        │
│     ▼           │  │ toolchain.py    │  │            │                      │
│ board_loader.py │  │ toolchain_      │  │            ▼                      │
│     │           │  │   esp32.py      │  │ Compilation:                      │
│     ▼           │  │ toolchain_      │  │ source_scanner.py                 │
│ mcu_specs.py    │  │   rp2040.py ... │  │ compiler.py / configurable_       │
│                 │  │     │           │  │ flag_builder.py                   │
│                 │  │     ▼           │  │            │                      │
│                 │  │ Frameworks:     │  │            ▼                      │
│                 │  │ arduino_core.py │  │ Linking:                          │
│                 │  │ framework_      │  │ linker.py / configurable_linker   │
│                 │  │   esp32.py ...  │  │ archive_creator.py                │
│                 │  │     │           │  │ binary_generator.py               │
│                 │  │     ▼           │  │            │                      │
│                 │  │ Libraries:      │  │            ▼                      │
│                 │  │ library_        │  │     firmware.hex/.bin             │
│                 │  │   manager.py    │  │                                   │
│                 │  │ library_        │  │ build_state.py (incremental)      │
│                 │  │   compiler.py   │  │                                   │
└─────────────────┘  └─────────────────┘  └───────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────────────┐
│                              DEPLOY LAYER                                   │
│  deployer.py (IDeployer)  deployer_esp32.py   monitor.py   qemu_runner.py  │
│         │                       │                 │              │          │
│         ▼                       ▼                 ▼              ▼          │
│     [avrdude]              [esptool]         [pyserial]      [Docker]       │
└─────────────────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────────────┐
│                              LEDGER LAYER                                   │
│  ledger/board_ledger.py          daemon/firmware_ledger.py                  │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Key Data Flows:**
1. **Build**: CLI → Daemon → Build Processor → Orchestrator → Compiler → Linker → firmware
2. **Deploy**: CLI → Daemon → Deploy Processor → Deployer (esptool/avrdude) → Device
3. **Packages**: Orchestrator → Cache → Downloader → fingerprint → extracted packages

## Critical Constraints

### Locking Strategy: Memory-Based Daemon Locks Only

**Do NOT use file-based locks** (`fcntl`, `msvcrt`, `.lock` files). All cross-process synchronization goes through the daemon's `ResourceLockManager`. Use `threading.Lock` for in-process synchronization only.

### Development Mode

Always set `FBUILD_DEV_MODE=1` when developing. This isolates:
- Daemon files → `.fbuild/daemon_dev/` (instead of `~/.fbuild/daemon/`)
- Cache files → `.fbuild/cache_dev/` (instead of `.fbuild/cache/`)

### Platform Requirements

- Python 3.10+
- Windows requires git-bash for shell scripts
- Type hints required for all functions
- Line length: 200 chars (configured in pyproject.toml)

## Test Organization

- `tests/unit/` - Fast unit tests, run in parallel with `-n auto`
- `tests/integration/` - Slow integration tests (use `--full` flag)
- `tests/{uno,esp32c6,esp32dev,...}/` - Hardware test projects with platformio.ini

Markers: `@pytest.mark.integration`, `@pytest.mark.concurrent_safety`, `@pytest.mark.hardware`

## Configuration Format

Uses standard platformio.ini with extensions:
- `extends = env:base` - Environment inheritance (multi-level supported)
- `${env:parent.key}` - Variable substitution
- `board_build.*` / `board_upload.*` - Board overrides
- `symlink://./path` - Local library symlinks (auto-converted to copies on Windows)
