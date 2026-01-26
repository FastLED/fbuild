# Serial Duplex Test for ESP32-S3

This test helps diagnose serial port locking issues when doing full-duplex communication (simultaneous read/write) with ESP32-S3 devices.

## Problem Description

When a host application tries to connect to a serial port and send/receive data simultaneously (full-duplex), the port can get locked with errors like:

```
A fatal error occurred: Could not open COM13, the port is busy or doesn't exist.
(could not open port 'COM13': PermissionError(13, 'Access is denied.', None, 5))
```

This typically happens when:
1. Another process has the port open
2. The port is in an inconsistent state from a previous connection
3. Full-duplex communication is not properly handled

## Files

1. **serial_duplex_test.ino** - ESP32-S3 sketch with simple JSON command protocol
2. **test_serial_duplex.py** - Python test script to interact with the sketch
3. **SERIAL_DUPLEX_TEST.md** - This file

## Protocol

The sketch implements a simple JSON-based command-response protocol:

**Request format:**
```json
{"cmd":"<command>","data":"<optional_data>"}
```

**Response format:**
```json
{"status":"ok","cmd":"<command>","response":"<response>"}
```

**Error format:**
```json
{"status":"error","error":"<error_type>","details":"<optional_details>"}
```

## Supported Commands

| Command | Data | Description | Response |
|---------|------|-------------|----------|
| `ping` | - | Simple echo test | `"pong"` |
| `info` | - | Get device info | CPU freq, LED state |
| `echo` | text | Echo back the data | Same as input |
| `led_on` | - | Turn LED on | `"LED is ON"` |
| `led_off` | - | Turn LED off | `"LED is OFF"` |
| `toggle` | - | Toggle LED state | Current state |
| `blink` | - | Blink LED once | `"blinked"` |

## Usage

### Step 1: Upload Sketch

Using fbuild:
```bash
# Build and upload
fbuild deploy tests/esp32s3 -e esp32s3 --sketch serial_duplex_test.ino

# Or specify port explicitly
fbuild deploy tests/esp32s3 -e esp32s3 --sketch serial_duplex_test.ino --port COM13
```

Using Arduino IDE:
1. Open `serial_duplex_test.ino`
2. Select board: ESP32-S3 Dev Module
3. Upload

### Step 2: Run Automated Tests

```bash
cd tests/esp32s3
python test_serial_duplex.py COM13
```

Expected output:
```
✓ Connected to COM13 at 115200 baud

============================================================
Running Serial Duplex Tests
============================================================

Test: ping
→ {"cmd": "ping"}
← {"status":"ok","cmd":"ping","response":"pong"}
  ✓ PASS

Test: info
→ {"cmd": "info"}
← {"status":"ok","cmd":"info","response":"ESP32-S3 @ 240 MHz, LED=OFF"}
  ✓ PASS

...

============================================================
Results: 7 passed, 0 failed
============================================================
```

### Step 3: Interactive Mode

For manual testing:
```bash
python test_serial_duplex.py COM13 --interactive
```

Example session:
```
> ping
→ {"cmd": "ping"}
← {"status":"ok","cmd":"ping","response":"pong"}
  ✓ pong

> echo Hello World
→ {"cmd": "echo", "data": "Hello World"}
← {"status":"ok","cmd":"echo","response":"Hello World"}
  ✓ Hello World

> led_on
→ {"cmd": "led_on"}
← {"status":"ok","cmd":"led_on","response":"LED is ON"}
  ✓ LED is ON

> quit
```

## Testing for Port Locking Issues

### Test 1: Basic Duplex Communication

Run the automated tests while monitoring the port state:

```bash
# Terminal 1: Run tests
python test_serial_duplex.py COM13

# Terminal 2: Check port availability (Windows)
mode COM13

# Terminal 2: Check port availability (Linux)
lsof | grep ttyUSB0
```

### Test 2: Rapid Connect/Disconnect

Test rapid connection cycles:

```bash
# Run tests multiple times in succession
for i in {1..10}; do
    echo "Test iteration $i"
    python test_serial_duplex.py COM13
    sleep 1
done
```

If port locking occurs, you'll see:
- `PermissionError(13, 'Access is denied.')`
- Port shows as busy even after script exits
- Need to manually reset device or unplug USB

### Test 3: Concurrent Access

Test what happens with multiple simultaneous connections:

```bash
# Terminal 1
python test_serial_duplex.py COM13 --interactive

# Terminal 2 (should fail with port busy)
python test_serial_duplex.py COM13 --interactive
```

Expected: Second connection should fail immediately with clear error message.

## Debugging Port Lock Issues

If the port gets locked:

1. **Check for orphaned processes:**
   ```bash
   # Windows (PowerShell)
   Get-CimInstance -ClassName Win32_PnPEntity | Where-Object {$_.Name -like "*USB*"} | Select-Object Name, Status

   # Linux
   lsof | grep ttyUSB0
   ps aux | grep python
   ```

2. **Kill orphaned processes:**
   ```bash
   # Windows
   taskkill /F /IM python.exe

   # Linux
   pkill -f test_serial_duplex.py
   ```

3. **Reset the port:**
   ```bash
   # Windows: Disable and re-enable in Device Manager
   # Or use devcon tool

   # Linux
   sudo chmod 666 /dev/ttyUSB0
   ```

4. **Reset the device:**
   - Unplug and replug USB cable
   - Press reset button on ESP32-S3

## Key Features for Port Locking Prevention

This sketch is designed to minimize port locking issues:

1. **No continuous output** - Only responds to commands, doesn't flood serial
2. **Proper flushing** - Calls `Serial.flush()` after each response
3. **Timeout handling** - Python script has read/write timeouts
4. **Clean shutdown** - Python script properly closes port on exit
5. **Buffer management** - Prevents buffer overflow with bounded command length

## Comparison with FastLED RPC-JSON

This sketch is inspired by the RPC-JSON protocol in FastLED but simplified:

**FastLED RPC-JSON:**
- Full C++ type-safe RPC framework
- Automatic function binding and argument conversion
- Complex nested JSON structures
- Prefix: `REMOTE: `

**This Test Sketch:**
- Simple command-response pattern
- Manual JSON parsing (no library overhead)
- Flat JSON structure
- No special prefix

**Why simplified?**
- Minimal dependencies (no ArduinoJson or FastLED required)
- Easier to understand and debug
- Focus on testing serial communication, not RPC features
- Smaller code size for faster upload during testing

## Troubleshooting

### No response from device

1. Check baud rate matches (115200)
2. Verify correct port
3. Try pressing reset button on device
4. Check for startup messages in serial monitor

### JSON parse errors

1. Ensure commands end with newline
2. Check for proper JSON syntax
3. Verify no extra characters in command

### Port permission denied

1. Close any other programs using the port (Arduino IDE, PlatformIO monitor, etc.)
2. Check port permissions (Linux: `sudo chmod 666 /dev/ttyUSB0`)
3. Try running as administrator (Windows)

### Device keeps resetting

1. Check power supply (USB cable may be insufficient)
2. Reduce serial traffic rate
3. Increase delays in test script

## Next Steps

After confirming this simple duplex communication works:

1. Compare with FastLED RPC-JSON implementation
2. Identify differences that may cause port locking
3. Test with increasing complexity (more frequent commands, larger payloads)
4. Monitor OS-level port state during full-duplex operation

## See Also

- FastLED Validation example: `~/dev/fastled3/examples/Validation/Validation.ino`
- FastLED JSON-RPC handler: `~/dev/fastled3/ci/util/json_rpc_handler.py`
- fbuild serial port handling: `fbuild/src/fbuild/daemon/shared_serial.py`
