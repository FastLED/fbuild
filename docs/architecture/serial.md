# Serial Manager Architecture

## SharedSerialManager

Central serial port access point. One instance per daemon. All serial I/O routes through this manager — no direct OS port access from other components.

### Concurrency Model

- **Per-port state**: `DashMap<String, SerialSession>` for lock-free reads
- **Background reader task**: `tokio::spawn` per open port, reads serial data continuously
- **Broadcast channel**: `tokio::sync::broadcast` distributes output to all attached readers
- **Exclusive writer**: Mutex-gated, one writer at a time per port with condition variable wait

### Session State

```rust
struct SerialSession {
    port: String,
    baud_rate: u32,
    is_open: bool,
    writer_client_id: Option<String>,      // exclusive
    reader_client_ids: HashSet<String>,     // shared
    output_buffer: VecDeque<String>,        // 10k lines circular
    owner_client_id: Option<String>,        // who opened
}
```

### Port Opening (Windows USB-CDC)

After device hard reset (flash or DTR toggle), Windows needs time to re-enumerate the USB device:

- **30 retries** (vs 15 on Linux/macOS)
- **Exponential backoff**: 1s → 2s → 4s → 8s → 10s max
- **Boot crash detection**: if crash patterns found in serial errors, trigger hardware reset immediately
- **USB-CDC write strategy v5**: aggressive input buffer draining, 50ms per-attempt timeout, DTR/RTS flow control toggling

### Background Reader Thread

```
loop {
    if serial.bytes_available() > 0 {
        data = serial.read_line()
        broadcast_to_all_readers(data)
        append_to_output_buffer(data)
        if crash_decoder_attached {
            process_crash_line(data)
        }
    } else {
        sleep(10ms)  // avoid busy-wait
    }
}
```

### Auto-Close

Port automatically closes when the last reader/writer detaches. Prevents resource leaks from orphaned sessions.

## WebSocket Message Protocol

Client → Server: `attach`, `write`, `detach`
Server → Client: `attached`, `data`, `preempted`, `reconnected`, `write_ack`, `error`

See `fbuild-serial/src/messages.rs` for exact types.
