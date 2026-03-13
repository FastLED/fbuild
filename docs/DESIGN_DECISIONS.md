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
