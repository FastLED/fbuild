# Source

## Modules

- **`lib.rs`** -- Crate root; declares public modules and documents all HTTP/WS endpoints
- **`main.rs`** -- Daemon binary entry point; sets up axum router, spawns background maintenance task, handles graceful shutdown
- **`context.rs`** -- `DaemonContext` (shared state), `BroadcastHub`, self-eviction/idle timeout constants
- **`device_manager.rs`** -- `DeviceManager` with exclusive/monitor leases, preemption, and stale device cleanup
- **`models.rs`** -- Request/response serde types for all API endpoints (build, deploy, monitor, devices, locks, reset)
- **`status_manager.rs`** -- `StatusManager` for atomic read-modify-write of `daemon_status.json`
- **`handlers/`** -- HTTP and WebSocket route handler modules
