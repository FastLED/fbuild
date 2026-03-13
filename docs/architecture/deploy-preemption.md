# Deploy Preemption Protocol

## State Machine

```
MONITOR ATTACHED (reading lines)
        │
    Deploy starts
        │
        ▼
PREEMPTION PHASE
  1. Force-close serial session in SharedSerialManager
  2. All readers receive "preempted" message via WebSocket
  3. auto_reconnect=true → pause (don't raise exception)
  4. auto_reconnect=false → raise MonitorPreemptedException
        │
        ▼
FLASH PHASE
  1. esptool/avrdude acquires exclusive OS serial handle
  2. Flash firmware
  3. Reset device (RTS/DTR sequence)
  4. Release serial handle
        │
        ▼
POST-DEPLOY PHASE
  1. Clear preemption notification
  2. 2-second delay for USB-CDC driver + device reboot
  3. if monitor_after: re-open port, send "reconnected" message
        │
        ▼
MONITOR RESUMED (seamless to client)
```

## Timing

| Step | Duration | Notes |
|------|----------|-------|
| Preemption notify | <10ms | WebSocket message to all readers |
| Serial close | <100ms | Force-close, no graceful flush |
| Flash (ESP32) | 10-30s | Depends on firmware size |
| Flash (AVR) | 2-5s | Smaller firmware |
| USB re-enumeration | 1-30s | Windows worst case; Linux <2s |
| Boot sequence | 1-3s | Bootloader + app init |
| Monitor reconnect | <1s | Auto-reattach after preemption clear |

## FastLED Integration

FastLED sets `monitor_after=False` in deploy requests and manages serial attachment itself:

```python
# Phase 1: Build
conn.build(clean=False, verbose=True)

# Phase 2: Deploy (no auto-monitor)
conn.deploy(port=upload_port, skip_build=True, monitor_after=False)

# Phase 3: Custom RPC attach
with SerialMonitor(port=monitor_port, baud_rate=115200, auto_reconnect=True) as mon:
    await asyncio.sleep(3.0)      # boot_wait
    drain_boot_output()            # clear stale buffer
    mon.write('{"method":"ping","params":[{}],"id":1}\n')
    # Wait for: REMOTE: {"id":1,"result":{...}}
```

The 3-second boot wait and boot output draining are critical for reliable JSON-RPC communication after flash.
