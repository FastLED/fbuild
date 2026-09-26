# Source

## Modules

- **`lib.rs`** -- Crate root; registers the `_native` PyO3 module and standalone factories/helpers including `connect_daemon()` and canonical `find_firmware()` artifact discovery
- **`ws_session.rs`** -- WebSocket session plumbing for the sync `SerialMonitor`: `WsSession::read_lines` and `WsSession::request` share the socket's read half, and `ReadYield` lets a `write`/`in_waiting` take it from an in-flight read, so writes never wait out a read's timeout (FastLED/fbuild#1431)
