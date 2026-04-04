# Source

## Modules

- **`lib.rs`** -- Crate root; defines `SerialMonitor` (WebSocket-based serial I/O), `Daemon` (lifecycle management), `DaemonConnection` (build/deploy/monitor operations), and `connect_daemon()` factory; registers the `_native` PyO3 module
