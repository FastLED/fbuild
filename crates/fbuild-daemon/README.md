# fbuild-daemon

Axum-based HTTP/WebSocket daemon server that replaces the Python FastAPI daemon. Maintains full API compatibility with the same endpoints and JSON schemas.

## Key Types

- `DaemonContext` -- shared state for all handlers: serial manager, device manager, project locks, status, broadcast hub
- `BroadcastHub` -- tokio broadcast channels for `/ws/status` and `/ws/logs` subscribers
- `DeviceManager` -- in-memory device lease manager with exclusive/monitor lease types and preemption
- `StatusManager` -- persistent `daemon_status.json` writer with atomic updates
- `OperationResponse` -- generic JSON response for build/deploy/monitor results

## Modules

- **context** -- `DaemonContext`, `BroadcastHub`, self-eviction and idle timeout constants
- **device_manager** -- `DeviceManager`, `DeviceLease`, `LeaseType`, `DeviceState`
- **handlers** -- HTTP and WebSocket route handlers (health, operations, devices, locks, websockets)
- **models** -- Request/response JSON types matching the Python daemon API contract
- **status_manager** -- `StatusManager`, `DaemonStatus`, `OperationInfo`

## Endpoints

Operations: `POST /api/build`, `/api/deploy`, `/api/test-emu`, `/api/monitor`, `/api/install-deps`, `/api/reset`

Management: `GET /health`, `/api/daemon/info`; `POST /api/daemon/shutdown`

Devices: `POST /api/devices/list`, `/api/devices/{port}/lease`, `/release`, `/preempt`; `GET /api/devices/{port}/status`

Locks: `GET /api/locks/status`; `POST /api/locks/clear`

WebSocket: `/ws/serial-monitor`, `/ws/status`, `/ws/logs`, `/ws/monitor/{session_id}`

See `docs/architecture/overview.md` and `docs/architecture/runtime.md` for architecture details.

## Why not the running-process broker

fbuild uses the `running-process` crate for process containment only
(`core` feature). Adopting its broker/BackendHandle daemon-control layer was
evaluated and declined (zackees/running-process#384):

- **Transport mismatch** — the broker discovers and routes backends over
  local sockets / named pipes; fbuild-daemon serves HTTP over loopback TCP
  (axum). Multiplexing the broker's nonce probe onto the HTTP listener would
  require a second raw listener.
- **Equivalent guarantees exist** — `GET /health` returns the daemon pid and
  `source_mtime`, covering the liveness and stale-daemon detection a
  BackendHandle probe would provide, and the CLI self-heals via
  `ensure_daemon_running()`.

Revisit only if daemon RPC ever moves off HTTP or broker-managed lifecycle
becomes desirable.
