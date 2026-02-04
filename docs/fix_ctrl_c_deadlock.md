# Fix: CTRL-C Deadlock in HTTP Client (v1.4.3)

## Problem

The fbuild client would hang indefinitely when the daemon didn't respond to HTTP requests, and **CTRL-C had no effect**. The user had to forcefully kill the client process externally.

### Root Cause

The client uses synchronous `httpx.Client.post()` calls that block in Windows socket I/O operations. When the daemon hangs or doesn't respond:

1. The client waits for up to 30 minutes (default timeout)
2. During this blocking wait, `KeyboardInterrupt` signals are **not delivered properly** on Windows
3. The user presses CTRL-C, but the client ignores it because it's stuck in kernel-level socket read
4. The client only responds if the process is forcefully killed externally

### Stack Trace Evidence

```python
File "C:\...\httpcore\_backends\sync.py", line 126, in read
    with map_exceptions(exc_map):
         ~~~~~~~~~~~~~~^^^^^^^^^
KeyboardInterrupt
```

The KeyboardInterrupt is raised deep in the HTTP client's socket read operation, but it doesn't propagate properly to allow the client to exit.

## Solution

### Interruptible HTTP Client Wrapper

Created a new module `src/fbuild/daemon/client/interruptible_http.py` that wraps `httpx` calls with proper interrupt handling:

**How it works:**

1. **Background Thread**: HTTP request runs in a background daemon thread
2. **Poll Loop**: Main thread polls for completion with short timeout intervals (0.5s)
3. **Interrupt Check**: Between polls, KeyboardInterrupt can be caught immediately
4. **Immediate Response**: When CTRL-C is pressed, the main thread exits within 0.5 seconds

**API:**

```python
from fbuild.daemon.client.interruptible_http import interruptible_post

response = interruptible_post(
    url="http://127.0.0.1:8765/api/build",
    json=request_data,
    timeout=1800.0,  # 30 minutes
)
```

### Updated Files

1. **New File**: `src/fbuild/daemon/client/interruptible_http.py`
   - `interruptible_post()` - POST requests with CTRL-C support
   - `interruptible_get()` - GET requests with CTRL-C support
   - `InterruptibleHTTPError` - Exception for HTTP failures

2. **Updated**: `src/fbuild/daemon/client/requests_http.py`
   - Replaced all `httpx.Client.post()` calls with `interruptible_post()`
   - Updated exception handling for `InterruptibleHTTPError`
   - Added "⚠️ Operation cancelled by user (CTRL-C)" messages
   - Removed unused `httpx` and `http_client` imports

3. **New Test**: `tests/unit/daemon/test_interruptible_http.py`
   - Tests successful requests
   - Tests keyboard interrupt behavior
   - Tests timeout handling
   - Tests connection errors

### Changed Functions

All daemon request functions now use interruptible HTTP:

- `request_build_http()` - Build requests
- `request_deploy_http()` - Deploy requests
- `request_monitor_http()` - Monitor requests
- `request_install_dependencies_http()` - Dependency installation

## User Experience Improvement

### Before (v1.4.2 and earlier)

```bash
$ fbuild build tests/esp32c6 -e esp32c6
📤 Submitting build request...
   ✅ Submitted

<deadlock - daemon hangs>
^C
^C
^C  # CTRL-C has no effect!
# User must open Task Manager and kill process
```

### After (v1.4.3+)

```bash
$ fbuild build tests/esp32c6 -e esp32c6
📤 Submitting build request...
   ✅ Submitted

<deadlock - daemon hangs>
^C
⚠️  Build cancelled by user (CTRL-C)
# Client exits immediately (within 0.5 seconds)
```

## Technical Details

### Windows Socket I/O Blocking

On Windows, synchronous socket operations (like `socket.recv()`) enter kernel space and cannot be interrupted by Python signals until the I/O completes or times out. This is a fundamental limitation of Windows socket handling.

### Threading Solution

By running the HTTP request in a background thread and polling from the main thread:

1. Main thread remains responsive to KeyboardInterrupt
2. Background thread can be abandoned (daemon thread)
3. No need to forcefully terminate sockets or threads
4. Clean, Pythonic solution that works cross-platform

### Poll Interval Tuning

The `check_interval` parameter (default 0.5s) balances:

- **Shorter intervals** (0.1s): More responsive to CTRL-C, but higher CPU usage
- **Longer intervals** (1.0s): Lower CPU usage, but slower interrupt response

The default 0.5s provides a good balance - users can interrupt within half a second.

## Testing

Run the test suite:

```bash
# Unit tests
uv run --group test pytest tests/unit/daemon/test_interruptible_http.py -v

# Test successful requests
pytest tests/unit/daemon/test_interruptible_http.py::test_interruptible_post_success -v

# Test keyboard interrupt (uses _thread.interrupt_main() to simulate CTRL-C)
pytest tests/unit/daemon/test_interruptible_http.py::test_interruptible_post_with_keyboard_interrupt -v

# Test timeout handling
pytest tests/unit/daemon/test_interruptible_http.py::test_interruptible_post_timeout -v
```

### Manual Testing

To manually verify CTRL-C behavior:

1. Start a build: `fbuild build tests/esp32c6 -e esp32c6`
2. Kill the daemon: `curl -X POST http://127.0.0.1:8765/api/daemon/shutdown`
3. The client should hang waiting for response
4. Press CTRL-C
5. **Expected**: Client exits within 0.5 seconds with "⚠️ Build cancelled by user (CTRL-C)"
6. **Before fix**: Client would hang indefinitely, ignore CTRL-C

## Future Improvements

Potential enhancements:

1. **Async HTTP Client**: Use `httpx.AsyncClient` with asyncio for native async support
2. **Progress Polling**: Add WebSocket status updates during long-running operations
3. **Configurable Poll Interval**: Allow users to configure `check_interval` via environment variable
4. **Cancellation Signal**: Send cancellation request to daemon when CTRL-C is pressed

## Related Issues

- Windows USB-CDC Serial Port Timeout (v1.3.36) - Similar Windows blocking I/O issue
- Daemon Spawn Race Condition (v1.3.31) - Daemon startup reliability

## References

- Stack Overflow: "Python KeyboardInterrupt not working with httpx on Windows"
- Python Issue #23057: "Windows socket operations don't respect signals"
- httpx Documentation: "Timeouts and Cancellation"
