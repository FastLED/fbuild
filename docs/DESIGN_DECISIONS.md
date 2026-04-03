# Design Decisions

## DD-001: Rust Rewrite (Full, Not Hybrid)

**Decision**: Full Rust rewrite with PyO3 bindings, not a hybrid Python+Rust approach.

**Context**: fbuild's Python implementation works but the serial driver system (SharedSerialManager, 1170 lines) has complex concurrency that Rust handles better. FastLED depends on `fbuild.api.SerialMonitor`.

**Rationale**: The test suite + PlatformIO fallback (`--platformio`) provide a safety net. An AI agent can A/B test behavior between platformio, python-fbuild, and rust-fbuild to converge on correctness. Full rewrite gives single-binary distribution and better concurrency.

**Consequences**: Must preserve exact Python API for SerialMonitor via PyO3. Windows USB-CDC timing quirks must be rediscovered in Rust.

## DD-002: Workspace Pattern from zccache

**Decision**: 11-crate Rust workspace with shared `Cargo.toml` dependencies, matching zccache's CI/trampoline/hook infrastructure.

**Context**: zccache has a proven pattern for Rust workspaces with uv-based toolchain management, agent hooks, and progressive disclosure documentation.

**Rationale**: Reusing the pattern avoids reinventing CI, toolchain management, and agent workflow. The trampoline solves the Chocolatey-vs-rustup PATH conflict on Windows.

## DD-003: Axum over Actix-Web

**Decision**: Use axum for the daemon HTTP server.

**Context**: The Python daemon uses FastAPI. Need a Rust equivalent with WebSocket support.

**Rationale**: Axum is tower-based (composable middleware), has first-class WebSocket support, integrates naturally with tokio. Actix-web uses its own runtime which conflicts with tokio-serial.

## DD-004: serialport Crate for Serial I/O

**Decision**: Use the `serialport` crate (v4) for cross-platform serial communication.

**Context**: Python fbuild uses pyserial. Need a Rust equivalent.

**Rationale**: `serialport` is the most mature cross-platform serial library in Rust. Supports Windows, Linux, macOS. Handles baud rate, DTR/RTS, timeouts. The USB-CDC retry logic must be reimplemented on top.

## DD-005: DashMap for Serial Sessions

**Decision**: Use `DashMap` for serial session state instead of `tokio::sync::RwLock<HashMap>`.

**Context**: SharedSerialManager needs concurrent access to per-port session state.

**Rationale**: DashMap provides sharded, lock-free reads. Multiple readers can check session state without blocking. Writers (port open/close) are rare compared to reads (buffer polling). Matches the Python implementation's threading.Lock pattern but with better read concurrency.

## DD-006: Broadcast Channel for Serial Output

**Decision**: Use `tokio::sync::broadcast` for distributing serial output to multiple readers.

**Context**: Python uses callback functions invoked by the reader thread. Need Rust equivalent.

**Rationale**: Broadcast channel naturally supports multiple receivers. Each reader gets its own `Receiver` via `subscribe()`. Backpressure via bounded channel (1024 messages). Matches the Python "broadcast to all readers" pattern without explicit callback management.

## DD-007: PyO3 with Internal Tokio Runtime

**Decision**: PyO3 `SerialMonitor` holds its own `tokio::Runtime` and uses `block_on()` for sync Python methods.

**Context**: FastLED calls `SerialMonitor.read_lines()` synchronously from Python. The Rust implementation needs async WebSocket communication.

**Rationale**: Creating a runtime per `SerialMonitor` instance is simple and avoids lifetime issues with shared runtimes. The runtime lives as long as the context manager. `block_on()` bridges sync Python to async Rust. FastLED's `ThreadPoolExecutor` wrapper handles the blocking nature.

## DD-008: compile_commands.json with Library Project Suppression

**Decision**: Generate `compile_commands.json` after every build. Suppress the project-root copy when `library.json` exists at the project root.

**Context**: clangd/VS Code IntelliSense needs a `compile_commands.json` at the project root to resolve `#include` paths. The old Python fbuild generates one with trampoline paths, which breaks "Go to Definition". Library projects (e.g. FastLED) have their own meson-based `compile_commands.json` that fbuild should not overwrite.

**Rationale**: The Rust port generates the compile database from the original `include_dirs` (not trampolines), so include paths always point to actual source directories. Detection via `library.json` (Arduino library manifest) is reliable — every Arduino library must have one, and sketch projects never do. Writing to the build directory is always safe; only the project-root copy is suppressed.

**Consequences**: Library developers building from their own repo keep their meson/cmake-generated `compile_commands.json`. Sketch projects get a clangd-compatible database automatically. The compile database is always available at `.fbuild/build/<env>/compile_commands.json` regardless.

## DD-009: Data-Driven Configuration over Hardcoded Values

**Decision**: All external URLs, versions, package metadata, and hardware-specific mappings must live in data files (JSON registries, board JSONs, config assets), never as constants or literals in Rust/Python source code.

**Context**: When adding ATTinyCore framework support, a hardcoded GitHub URL with a wrong tag prefix caused a 404 error on the very first run. PlatformIO avoids this by using data-driven manifests (`platform.json`, `package.json`, board JSONs) that map board cores to framework packages at runtime.

**Rationale**: Hardcoded values create maintenance burden — every upstream tag rename, version bump, or URL change requires a code change, a rebuild, and a release. Data files can be validated by CI (see `validate_boards` workflow), updated independently of code, and are easier to audit. PlatformIO's patterns are the reference implementation for how hardware metadata should be structured, even though we never invoke PlatformIO as a build tool.

**Rules**:
1. **Board definitions** → `crates/fbuild-config/assets/boards/json/` (enriched from PlatformIO, validated by CI)
2. **Framework packages** → `crates/fbuild-packages/assets/avr_frameworks.json` (maps core names to GitHub repos, versions)
3. **Toolchain URLs/versions** → data files or registry configs, not `const` in source
4. **Study PlatformIO** for data structure patterns; **never invoke PlatformIO** at runtime

**Consequences**: Adding a new board or framework is a data file edit, not a code change. CI catches drift between our cached board JSONs and PlatformIO's upstream definitions. The `validate_boards` GitHub Actions job ensures our assets stay synchronized.

## DD-007: Streaming Build Output via NDJSON

**Decision**: Build log lines are streamed from the daemon to the CLI in real-time using NDJSON (newline-delimited JSON) over the existing HTTP POST `/api/build` endpoint.

**Context**: fbuild runs builds in a daemon process. The CLI sends a build request and waits for the response. Previously, all build output (compilation steps, warnings, size info) was collected in memory and returned as a single message at the end. Users saw no output until the build completed, making long builds feel unresponsive.

**Rationale**:
- **NDJSON over SSE/WebSocket**: NDJSON is simpler to implement (no protocol overhead), works with standard HTTP streaming, and is easy to parse line-by-line. SSE requires GET (our endpoint is POST). WebSocket adds connection lifecycle complexity.
- **`stream` field for backwards compatibility**: The `BuildRequest.stream` boolean (default `false`) preserves the existing JSON response format for the Python client and MCP tools. Only the Rust CLI sets `stream: true`.
- **`BuildLog` struct with `VecDeque` + optional `Sender`**: Centralizes log collection in `fbuild-core`. Uses `std::sync::mpsc::Sender` (not tokio) so orchestrators have no async dependency. A bridge task in the daemon forwards sync channel messages to the async response stream.
- **Channel flow**: `Orchestrator → BuildLog (std::sync::mpsc) → bridge spawn_blocking → tokio::mpsc → NDJSON stream → reqwest bytes_stream → CLI println`

**Protocol**:
```
{"type":"log","message":"=== BUILDING uno ==="}
{"type":"log","message":"Compiling src/main.cpp.o"}
{"type":"result","success":true,"request_id":"...","message":"build succeeded in 1.5s","exit_code":0}
```

**Consequences**: The CLI now shows compilation progress, compiler warnings, and size info in real-time. The daemon's non-streaming path and Python API are unchanged. Future orchestrators (ESP32, Teensy) can adopt the same `BuildLog` pattern by setting `log_sender` in their `BuildParams`.
