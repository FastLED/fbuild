# Source

## Modules

- **`lib.rs`** -- Crate root; declares public modules and documents all HTTP/WS endpoints
- **`main.rs`** -- Daemon binary entry point; sets up axum router, spawns background maintenance task, handles graceful shutdown
- **`context.rs`** -- `DaemonContext` (shared state), `BroadcastHub`, self-eviction/idle timeout constants
- **`device_manager.rs`** -- `DeviceManager` with exclusive/monitor leases, preemption, and stale device cleanup
- **`models.rs`** -- Request/response serde types for all API endpoints (build, deploy, monitor, devices, locks, reset)
- **`shutdown.rs`** -- Exit paths: `refuse_new_operations_when_shutting_down` middleware (503 once shutdown starts), SIGTERM controlled exit (`exit_on_terminate`: drain in-flight operations up to 5 s, then flush), and `persist_and_clean_up` (pid/port/status cleanup + bounded zccache flush) shared by every clean exit
- **`startup.rs`** -- `StartupGate`: answers every request with `503 {"status":"starting","phase":...}` on a duplicate of the bound listener while `main` initializes, then hands the endpoint to the full router (FastLED/fbuild#1480)
- **`status_manager.rs`** -- `StatusManager` for atomic read-modify-write of `daemon_status.json`
- **`handlers/`** -- HTTP and WebSocket route handler modules
