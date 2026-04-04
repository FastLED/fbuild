# Crates

The fbuild workspace contains 11 crates.

| Crate | Purpose |
|---|---|
| `fbuild-core` | Core types, errors, and utilities (`FbuildError`, `Result`, `BuildProfile`, `Platform`) |
| `fbuild-config` | PlatformIO INI parser, board config, MCU specs |
| `fbuild-paths` | Dev/prod path isolation, port mapping, cache dirs |
| `fbuild-packages` | Package downloads, toolchain resolution, library manager |
| `fbuild-serial` | Shared serial manager, deploy preemption, WebSocket messages |
| `fbuild-build` | Build orchestration for AVR, ESP32, ESP8266, Teensy |
| `fbuild-deploy` | Firmware deployment via esptool/avrdude/picotool |
| `fbuild-daemon` | Axum HTTP/WebSocket server |
| `fbuild-cli` | Clap CLI: build, deploy, monitor, purge |
| `fbuild-python` | PyO3 bindings for Python integration |
| `fbuild-test-support` | Test utilities and fixtures |

See [CLAUDE.md](CLAUDE.md) for the dependency graph and design patterns.
