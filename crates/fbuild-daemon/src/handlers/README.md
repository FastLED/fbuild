# Handlers

HTTP and WebSocket route handlers for the fbuild daemon.

- **`mod.rs`** -- Module declarations
- **`health.rs`** -- `GET /`, `/health`, `/api/daemon/info`, `POST /api/daemon/shutdown`
- **`operations.rs`** -- `POST /api/build`, `/api/deploy`, `/api/monitor`, `/api/install-deps`, `/api/reset` with RAII `OperationGuard` for state tracking
- **`devices.rs`** -- Device discovery, lease acquire/release/preempt handlers for `/api/devices/` endpoints
- **`locks.rs`** -- `GET /api/locks/status` and `POST /api/locks/clear` for project and serial port locks
- **`emulator.rs`** -- Emulator deploy handlers (AVR8js, QEMU), `EmulatorRunner` trait abstraction, `POST /api/test-emu` build-then-emulate flow
- **`websockets.rs`** -- WebSocket upgrade handlers: serial monitor (`/ws/serial-monitor`), status streaming (`/ws/status`), log streaming (`/ws/logs`), named monitor sessions (`/ws/monitor/{session_id}`)
