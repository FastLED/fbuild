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

## running-process broker

fbuild uses the `running-process` broker for daemon discovery and versioned
backend launch while preserving the HTTP API as the operation transport.

When the broker launches `fbuild-daemon`, it provides
`RUNNING_PROCESS_BROKER_V1_BACKEND_PIPE`. The daemon binds that local socket in
`src/broker/backend.rs`, answers `BackendHandle` identity probes, and supports
broker-framed health/daemon-info requests over fbuild's registered payload
protocol (`0x7EB1`). That makes the broker-selected daemon process verifiable
before CLI/PyO3 callers continue through the existing loopback HTTP endpoints.

The current migration slice deliberately leaves build/deploy/monitor operations
on HTTP, including streaming NDJSON builds. `RUNNING_PROCESS_DISABLE=1` keeps
the legacy direct-spawn/direct-HTTP path for rollback, and broker-unavailable
cases fall back to that path unless the broker explicitly refuses the requested
fbuild version.
