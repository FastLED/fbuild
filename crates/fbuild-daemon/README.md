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
