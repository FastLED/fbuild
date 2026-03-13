# Data Flows

## Build Request

```
1. CLI parses args → BuildRequest JSON
2. HTTP POST /api/operations/build → daemon
3. Daemon acquires project lock
4. BuildRequestProcessor:
   a. Parse platformio.ini → PlatformIOConfig
   b. Detect platform → select orchestrator
   c. Resolve toolchain package → download if needed
   d. Resolve library packages → download if needed
   e. Scan sources → compute dependency graph
   f. Parallel compile (--jobs N) → object files
   g. Link → ELF binary
   h. objcopy → HEX/BIN firmware
   i. Size report → SizeInfo
5. Daemon releases lock, returns BuildResult
6. CLI displays result + size info
```

## Deploy Request

```
1. CLI parses args → DeployRequest JSON
2. HTTP POST /api/operations/deploy → daemon
3. DeployRequestProcessor:
   a. Optional: build firmware (skip if skip_build=true)
   b. Notify serial monitors: preemption starting
   c. Force-close serial sessions on target port
   d. Invoke deployer (esptool/avrdude/picotool)
   e. Wait for flash + device reset
   f. Clear preemption notification
   g. Optional: start monitor (if monitor_after=true)
      - 2s USB-CDC re-enumeration delay
      - Open serial port (30 retries on Windows)
      - Attach as monitor reader
4. CLI displays result
```

## Serial Monitor (WebSocket)

```
1. Client connects: GET /ws/serial-monitor
2. Client sends: { "type": "attach", "port": "COM13", "baud_rate": 115200 }
3. Daemon opens port if needed, attaches reader
4. Background reader task: serial → broadcast channel → all readers
5. Daemon pushes: { "type": "data", "lines": [...], "current_index": N }
6. Client sends: { "type": "write", "data": "base64..." }
7. Daemon acquires writer lock, writes to port, sends write_ack
8. On deploy preemption: daemon sends { "type": "preempted" }
9. After deploy: daemon sends { "type": "reconnected" }
10. Client sends: { "type": "detach" } → cleanup
```
