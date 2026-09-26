# Source

## Modules

- **`lib.rs`** -- Crate root; registers the `_native` PyO3 module and standalone factories/helpers including `connect_daemon()` and canonical `find_firmware()` artifact discovery
- **`serial_session.rs`** -- Shared, PyO3-free per-session reader-task core for both `SerialMonitor` and `AsyncSerialMonitor` (FastLED/fbuild#1485). One reader task per session owns the WebSocket read half exclusively and dispatches to a reply FIFO (`write_ack`/`in_waiting`), an RPC-reply FIFO (`REMOTE:` lines), a bounded line queue (`Notify`), and a `watch::Sender<SessionStatus>` for port events. Replaced the #1431/#1484 polling hand-off (`ws_session::ReadYield`/`RpcRoute`). Contains the fake-daemon Rust acceptance tests (AT-1..AT-20).
- **`serial_monitor.rs`** -- Synchronous `SerialMonitor` PyO3 binding: the API FastLED depends on. A thin facade over `serial_session::SerialSession`; every blocking call releases the GIL (`py.detach`) and runs `rt.block_on(session.op(...))`. Adds the additive `interrupt_reads()` method (FastLED #3219).
- **`async_serial_monitor.rs`** -- Asynchronous `AsyncSerialMonitor` PyO3 binding, promoted to a supported API by FastLED/fbuild#1485 §4.7 (no longer experimental). A thin facade over the same `SerialSession` core; every awaitable comes from `pyo3_async_runtimes::tokio::future_into_py`.
- **`json_rpc.rs`** -- `REMOTE:` JSON-RPC response-line helper (`extract_remote_json_rpc_response`, test-only) shared by the serial-session core and its facades. The async read/write loops that used to live here were deleted once both facades moved onto `serial_session::SerialSession`.
- **`messages.rs`** -- Shared WebSocket message types (`ClientMessage`, `ServerMessage`) and type aliases (`WsSink`, `WsSource`) used by the serial-session core.
- **`daemon.rs`** / **`daemon_connection.rs`** / **`async_daemon_connection.rs`** -- `Daemon`/`AsyncDaemon` and `DaemonConnection`/`AsyncDaemonConnection` PyO3 bindings: thin HTTP clients to the daemon's build/deploy/monitor endpoints.
- **`outcome.rs`** -- `OperationOutcome` parsing shared by the sync and async daemon-connection surfaces.
