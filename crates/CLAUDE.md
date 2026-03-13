# Crates Architecture

## Dependency Graph

```
fbuild-daemon (bin) ──────────────────────────────────────────┐
  ├─ fbuild-build ─── fbuild-config ─── fbuild-core          │
  │                ├── fbuild-paths ─── fbuild-core           │
  │                └── fbuild-packages ─── fbuild-paths       │
  ├─ fbuild-deploy ── fbuild-serial ─── fbuild-core           │
  │                ├── fbuild-config                          │
  │                └── fbuild-paths                           │
  ├─ fbuild-serial ── fbuild-core (DashMap, broadcast, tokio) │
  └─ fbuild-packages ── fbuild-config, fbuild-paths           │
                                                              │
fbuild-cli (bin: "fbuild") ──────────────────────────────────┤
  ├─ fbuild-core                                              │
  ├─ fbuild-config                                            │
  └─ fbuild-paths                                             │
                                                              │
fbuild-python (cdylib: PyO3 bindings) ───────────────────────┤
  ├─ fbuild-core                                              │
  ├─ fbuild-serial                                            │
  └─ fbuild-paths                                             │
                                                              │
fbuild-test-support (test utilities) ────────────────────────┘
```

## Crate Responsibilities

- **fbuild-core** — `FbuildError`/`Result`, `BuildProfile`, `Platform`, `SizeInfo`, `DaemonState`
- **fbuild-config** — `PlatformIOConfig` (INI parser with `extends` inheritance), `BoardConfig`, `McuSpec`
- **fbuild-paths** — Dev/prod path isolation (`~/.fbuild/{dev|prod}/`), port mapping (8765/8865), cache dirs
- **fbuild-packages** — URL-based package downloads, toolchain resolution, library manager, parallel pipeline
- **fbuild-serial** — `SharedSerialManager` (centralized serial I/O), deploy preemption protocol, WebSocket messages, USB-CDC retry logic
- **fbuild-build** — `BuildOrchestrator` trait, per-platform orchestrators (AVR, ESP32, ESP8266, RP2040, STM32, Teensy, WASM)
- **fbuild-deploy** — `Deployer` trait, esptool/avrdude/picotool invocation, firmware upload
- **fbuild-daemon** — Axum HTTP/WS server, request processors, device lease manager, lock manager
- **fbuild-cli** — Clap CLI: build, deploy, monitor, purge subcommands. Thin HTTP client to daemon.
- **fbuild-python** — PyO3 bindings: `SerialMonitor`, `Daemon`, `DaemonConnection`, `connect_daemon`
- **fbuild-test-support** — `create_test_project()`, temp dir fixtures

## Key Design Patterns

**Daemon-centric:** All serial access routes through `SharedSerialManager` in the daemon. No OS-level port locks. Multiple readers (broadcast), exclusive writer (condition variable).

**Deploy preemption:** Deploy forcibly closes serial sessions → flash via esptool → 2s USB re-enumeration delay → monitors auto-reconnect. Monitors with `auto_reconnect=true` pause during preemption.

**HTTP API boundary:** CLI sends JSON requests to daemon over HTTP. Build output streams via WebSocket. Serial monitor data streams via `/ws/serial-monitor`. All endpoints match the Python FastAPI daemon's contract.

**PyO3 consumer contract:** FastLED imports `SerialMonitor` as a Python context manager with `read_lines()`, `write()`, `write_json_rpc()`. The `fbuild-python` crate must preserve this API exactly.

## Current Status

Workspace scaffolded with all crate dependencies, types, and traits. All 11 crates compile clean (clippy `-D warnings`). Implementation starts with fbuild-serial (highest risk, most complex).
