# System Overview

## Architecture Diagram

```
┌──────────────┐     HTTP/WS      ┌─────────────────────────────────────┐
│  fbuild-cli  │ ───────────────► │           fbuild-daemon             │
│  (binary)    │                  │  ┌────────────┐ ┌───────────────┐   │
└──────────────┘                  │  │ Build Proc │ │ Deploy Proc   │   │
                                  │  └─────┬──────┘ └──────┬────────┘   │
┌──────────────┐     PyO3         │        │               │            │
│ fbuild-python│ ── bindings ──►  │  ┌─────▼──────┐ ┌──────▼────────┐   │
│ (cdylib)     │  SerialMonitor   │  │fbuild-build│ │ fbuild-deploy │   │
└──────────────┘     WS API       │  └─────┬──────┘ └──────┬────────┘   │
                                  │        │               │            │
                                  │  ┌─────▼──────────────▼─────────┐   │
                                  │  │     fbuild-serial            │   │
                                  │  │  SharedSerialManager         │   │
                                  │  │  Preemption + USB-CDC        │   │
                                  │  └──────────────────────────────┘   │
                                  └─────────────────────────────────────┘
                                           │              │
                                  ┌────────▼───┐  ┌───────▼────────┐
                                  │ fbuild-    │  │ fbuild-        │
                                  │ packages   │  │ config         │
                                  └────────┬───┘  └───────┬────────┘
                                           │              │
                                  ┌────────▼──────────────▼────────┐
                                  │       fbuild-core / paths      │
                                  └────────────────────────────────┘
```

## Components

### 1. CLI (`fbuild-cli`)
Thin clap-based CLI. Parses arguments, sends HTTP requests to daemon, streams output. Subcommands: `build`, `deploy`, `monitor`, `purge`.

### 2. Daemon (`fbuild-daemon`)
Axum HTTP/WS server (port 8765 prod, 8865 dev). Processes build/deploy/monitor requests. Manages device leases, configuration locks, and serial sessions. Single daemon shared across all projects.

### 3. Build Orchestrators (`fbuild-build`)
Platform-specific build logic behind `BuildOrchestrator` trait. Each platform (AVR, ESP32, ESP8266, RP2040, STM32, Teensy, WASM) has its own orchestrator handling: source scanning, compiler flag generation, parallel compilation, linking, size reporting.

### 4. Deploy (`fbuild-deploy`)
`Deployer` trait with platform-specific upload tools: esptool (ESP32), avrdude (AVR), picotool (RP2040), st-flash (STM32), teensy_loader_cli (Teensy).

### 5. Serial Manager (`fbuild-serial`)
Central serial port management. All I/O goes through `SharedSerialManager` in the daemon. Background reader tasks per port. Broadcast channel to readers, exclusive writer via condition variable. Deploy preemption protocol for flash → monitor handoff. See [serial.md](serial.md).

### 6. PyO3 Bindings (`fbuild-python`)
Exposes `SerialMonitor`, `Daemon`, `DaemonConnection`, `connect_daemon` to Python via PyO3. FastLED imports these for JSON-RPC communication with devices. See [pyo3-bindings.md](pyo3-bindings.md).

### 7. Config (`fbuild-config`)
Parses platformio.ini with environment inheritance (`extends = env:parent`), variable substitution (`${env:parent.key}`), and board overrides.

### 8. Packages (`fbuild-packages`)
URL-based package management. Downloads toolchains and libraries to `~/.fbuild/{dev|prod}/cache/`. Parallel download pipeline with Docker pull-style TUI.

### 9. Paths (`fbuild-paths`)
Single source of truth for all `.fbuild` paths. Respects `FBUILD_DEV_MODE=1` for dev/prod isolation.
