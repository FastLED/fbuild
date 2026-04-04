# fbuild-serial

Centralized serial port I/O manager for the fbuild daemon. All serial access routes through this crate -- no OS-level port locks. Provides broadcast readers, exclusive writer access, deploy preemption protocol, and crash stack trace decoding.

## Key Types

- `SharedSerialManager` -- one-per-daemon manager: opens/closes ports, spawns background reader tasks, distributes output via broadcast channels, manages writer exclusivity
- `SerialSession` -- per-port state: serial handle, reader/writer tracking, output buffer, stop flag
- `PreemptionTracker` -- tracks which ports are preempted by deploy operations
- `CrashDecoder` -- state machine that intercepts ESP32 crash dumps and decodes them via `addr2line`
- `SerialClientMessage` / `SerialServerMessage` -- WebSocket protocol enums (attach, write, detach / attached, data, preempted, write_ack, error)
- `PortSessionInfo` -- snapshot of a serial session for status reporting

## Modules

- **manager** -- `SharedSerialManager`, port open/close with USB-CDC retry, read/write, preemption integration
- **session** -- `SerialSession` state struct
- **messages** -- Serde-tagged WebSocket message types matching the Python protocol
- **preemption** -- `PreemptionTracker` for deploy preemption lifecycle
- **crash_decoder** -- `CrashDecoder` for Xtensa/RISC-V crash dumps, `derive_addr2line_path`

See `docs/architecture/serial.md` and `docs/architecture/deploy-preemption.md` for architecture details.
