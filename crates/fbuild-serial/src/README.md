# Source

## Modules

- **`lib.rs`** -- Crate root; re-exports `SharedSerialManager`, `PortSessionInfo`, `SerialClientMessage`, `SerialServerMessage`, `SerialSession`
- **`manager.rs`** -- `SharedSerialManager`: port open with retry/backoff, background reader task, broadcast output, writer lock, preemption, crash decoder integration
- **`session.rs`** -- `SerialSession`: per-port state including serial handle, reader/writer client tracking, output buffer, byte counters
- **`messages.rs`** -- `SerialClientMessage` (attach/write/detach) and `SerialServerMessage` (attached/data/preempted/reconnected/write_ack/error) serde enums
- **`preemption.rs`** -- `PreemptionTracker`: async hashmap of preempted ports with reason and timestamp
- **`crash_decoder.rs`** -- `CrashDecoder` state machine: crash start detection, address extraction (Xtensa backtrace, RISC-V registers, abort PC), addr2line invocation, debouncing
