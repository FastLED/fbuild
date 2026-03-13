# TODO

## Current: Phase 1 — Serial Manager

- [ ] Implement `SharedSerialManager` with real `serialport` I/O
- [ ] Background reader task (tokio::spawn per port)
- [ ] Broadcast channel output distribution
- [ ] Exclusive writer with Mutex
- [ ] Windows USB-CDC retry logic (30 retries, exponential backoff)
- [ ] Deploy preemption protocol
- [ ] WebSocket message handling
- [ ] Unit tests with mock serial

## Next: Phase 2 — Daemon Server

- [ ] Axum router with all endpoints
- [ ] WebSocket serial monitor handler
- [ ] Request processor framework
- [ ] Device lease manager
- [ ] Daemon lifecycle
