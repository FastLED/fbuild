# Runtime & Concurrency

## Daemon Concurrency Model

The daemon runs on a multi-threaded tokio runtime:

- **HTTP/WS requests**: Each request is a tokio task (via axum)
- **Serial readers**: One `tokio::spawn` per open serial port
- **Build operations**: Can run concurrently for different projects, serialized for same project via lock
- **Deploy operations**: Serialized per port (preemption protocol)

## Lock Strategy

Request-level synchronization is in-memory within the daemon process — no
file-based locks are used for builds, serial, deploy, or config state.

| Resource | Lock Type | Scope |
|----------|-----------|-------|
| Project build | Per-project Mutex | Prevents concurrent builds of same project |
| Serial port writer | Per-port Mutex | Exclusive write access |
| Serial port readers | DashMap | Lock-free concurrent reads via broadcast |
| Device lease | Per-device RwLock | Exclusive (deploy) or shared (monitor) |
| Config lock | Per-project Mutex | Prevents concurrent config changes |

The one exception is daemon startup/lifetime ownership itself, which is
process-level rather than request-level and so can't be arbitrated by an
in-memory manager (there may be no daemon process yet). `fbuild-paths`'s
`daemon_ownership` module (soldr-style, FastLED/fbuild#1159) provides a
version-blind `root-owner.lock` that a daemon holds for its whole lifetime,
plus a `spawn.lock` single-flight election so multiple CLI invocations racing
to start a daemon don't spawn duplicates. `fbuild clean cache` acquires
`root-owner.lock` exclusively (after verifying and displacing any daemon,
including legacy ones, that still owns it) before deleting the zccache store.
These are OS-released file locks, never manually broken or deleted, and they
never gate zccache object reads/writes — those stay in-memory-synchronized
inside zccache itself.

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
