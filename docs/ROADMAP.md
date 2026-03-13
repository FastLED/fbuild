# Implementation Roadmap

## Phase 0: Scaffolding ✅ COMPLETE

- [x] Workspace with 11 crates
- [x] All Cargo.toml with correct dependencies
- [x] Core types, errors, enums
- [x] CI infrastructure (lint, test, install, hooks)
- [x] Progressive disclosure documentation
- [x] PyO3 binding stubs (SerialMonitor, Daemon, connect_daemon)
- [x] `uv run cargo check/test/clippy` all passing

## Phase 1: Serial Manager (HIGHEST RISK)

- [ ] `fbuild-serial`: Implement `SharedSerialManager` with real `serialport` crate
- [ ] Background reader task with `tokio::spawn`
- [ ] Broadcast channel for output distribution
- [ ] Exclusive writer with condition variable
- [ ] Port opening with Windows USB-CDC retry logic (30 retries, backoff)
- [ ] Preemption protocol (force-close, notify, reconnect)
- [ ] WebSocket message serialization/deserialization
- [ ] Unit tests with mock serial ports

## Phase 2: Daemon Server

- [ ] Axum HTTP server with health/info/shutdown endpoints
- [ ] WebSocket endpoint for serial monitor API
- [ ] Request processor framework (build, deploy, monitor)
- [ ] Device lease manager (exclusive/monitor leases)
- [ ] Configuration lock manager
- [ ] Daemon lifecycle (auto-start, graceful shutdown, signal file)

## Phase 3: Config & Paths

- [ ] platformio.ini parser with environment inheritance
- [ ] Variable substitution (`${env:parent.key}`)
- [ ] Board config from JSON definitions
- [ ] MCU spec database
- [ ] Full path resolution matching Python implementation

## Phase 4: Package Management

- [ ] URL-based package downloader
- [ ] Toolchain resolution and extraction
- [ ] Library dependency resolution
- [ ] Parallel download pipeline with progress TUI
- [ ] Cache management (purge, size reporting)

## Phase 5: Build Orchestrators

- [ ] `BuildOrchestrator` trait implementation for each platform
- [ ] Source scanning and dependency graph
- [ ] Parallel compilation with job control
- [ ] Build profiles (release with LTO, quick without)
- [ ] Size reporting (text, data, bss, flash%, ram%)
- [ ] Platforms: AVR, ESP32, ESP8266, RP2040, STM32, Teensy, WASM

## Phase 6: Deploy & Monitor

- [ ] Deployer implementations (esptool, avrdude, picotool, st-flash)
- [ ] Deploy → serial preemption integration
- [ ] Monitor processor (shared and direct modes)
- [ ] Crash decoder integration

## Phase 7: PyO3 Bindings & Integration

- [ ] `SerialMonitor` with real WebSocket backend
- [ ] `DaemonConnection` with real HTTP backend
- [ ] `Daemon` lifecycle methods
- [ ] maturin build integration
- [ ] FastLED integration tests passing
- [ ] Python fbuild integration tests passing against Rust daemon
