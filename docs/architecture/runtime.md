# Runtime & Concurrency

## Daemon Concurrency Model

The daemon runs on a multi-threaded tokio runtime:

- **HTTP/WS requests**: Each request is a tokio task (via axum)
- **Serial readers**: One `tokio::spawn` per open serial port
- **Build operations**: Can run concurrently for different projects, serialized for same project via lock
- **Deploy operations**: Serialized per port (preemption protocol)

## Lock Strategy

All synchronization is in-memory within the daemon process. No file-based locks.

| Resource | Lock Type | Scope |
|----------|-----------|-------|
| Project build | Per-project Mutex | Prevents concurrent builds of same project |
| Serial port writer | Per-port Mutex | Exclusive write access |
| Serial port readers | DashMap | Lock-free concurrent reads via broadcast |
| Device lease | Per-device RwLock | Exclusive (deploy) or shared (monitor) |
| Config lock | Per-project Mutex | Prevents concurrent config changes |

## Error Recovery

- **Daemon crash**: CLI detects connection failure, restarts daemon automatically
- **Build failure**: Processor catches error, returns structured error response
- **Serial disconnect**: Background reader detects, notifies readers, auto-reconnect if enabled
- **Deploy timeout**: esptool/avrdude have their own timeouts; daemon adds outer timeout
- **Port stuck**: Kill orphaned processes on port before new session

## Shutdown

1. HTTP POST `/api/daemon/shutdown` (preferred)
2. Signal file: `touch ~/.fbuild/{dev|prod}/daemon/shutdown.signal`
3. Specific PID kill (never `pkill python` or `taskkill /IM` — shared daemon!)
