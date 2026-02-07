# Daemon & IPC Reference

> Reference doc for Claude Code. Read when modifying daemon endpoints or client code.

## HTTP/WebSocket Architecture (as of v1.3.28)

**The daemon uses FastAPI for all client-daemon communication**, replacing the previous file-based IPC system. This provides:

- **Better performance**: No file system polling, instant request handling
- **Real-time updates**: WebSocket streaming for build output, monitor sessions, logs
- **Better error handling**: HTTP status codes, structured error responses
- **Standard tooling**: OpenAPI docs, type validation via Pydantic
- **Cleaner codebase**: Removed ~500 lines of file polling logic

## Communication Endpoints

**HTTP REST API** (http://127.0.0.1:8765 in production, port 8865 in dev mode):
```
POST /api/build              - Build a project
POST /api/deploy             - Deploy firmware to device
POST /api/monitor            - Start serial monitor
POST /api/install-deps       - Install dependencies
GET  /api/devices/list       - List connected devices
GET  /api/devices/{id}       - Get device status
POST /api/devices/{id}/lease - Acquire device lock
POST /api/locks/status       - Get lock status
GET  /api/daemon/status      - Daemon health check
POST /api/daemon/shutdown    - Graceful shutdown
```

**WebSocket Endpoints**:
```
WS /ws/status               - Real-time build/deploy status updates
WS /ws/monitor/{session_id} - Bidirectional serial monitor session (CLI usage)
WS /ws/serial-monitor       - Serial Monitor API endpoint (fbuild.api.SerialMonitor)
WS /ws/logs                 - Live daemon log streaming
```

## WebSocket Serial Monitor API Protocol (`/ws/serial-monitor`)

```
Client → Server:
  {"type": "attach", "client_id": "...", "port": "COM13", "baud_rate": 115200, "open_if_needed": true}
  {"type": "write", "data": "base64_encoded_data"}
  {"type": "detach"}
  {"type": "ping"}

Server → Client:
  {"type": "attached", "success": true, "message": "..."}
  {"type": "data", "lines": ["line1", "line2"], "current_index": 42}
  {"type": "preempted", "reason": "deploy", "preempted_by": "..."}
  {"type": "reconnected", "message": "..."}
  {"type": "write_ack", "success": true, "bytes_written": 10}
  {"type": "error", "message": "..."}
  {"type": "pong", "timestamp": 1234567890.123}
```

## Client Utilities

- `src/fbuild/daemon/client/http_utils.py` - HTTP client helpers, port discovery
- `src/fbuild/daemon/client/requests_http.py` - HTTP request functions
- `src/fbuild/daemon/client/devices_http.py` - Device management via HTTP
- `src/fbuild/daemon/client/locks_http.py` - Lock management via HTTP
- `src/fbuild/daemon/client/websocket_client.py` - WebSocket client helpers
- `src/fbuild/api/serial_monitor.py` - WebSocket-based SerialMonitor API (v2)
- `src/fbuild/api/serial_monitor_file.py` - File-based SerialMonitor API (deprecated, v1)

## Migration Status

- Build, deploy, monitor, install-deps operations → HTTP
- Device management (list, status, lease, release, preempt) → HTTP
- Lock management (status, clear) → HTTP
- Daemon status and shutdown → HTTP
- Real-time status updates → WebSocket
- Serial monitor sessions → WebSocket
- Daemon log streaming → WebSocket
- Serial Monitor API (fbuild.api.SerialMonitor) → **Fully migrated to WebSocket** (as of iteration 9)
  - **Status**: Async handling fixed - attach/detach operations work correctly
  - **Implementation**: Uses concurrent message receiver, processor, and data pusher tasks
  - **Performance**: Real-time data streaming with <10ms latency (vs 100ms file-based polling)
  - **Note**: File-based handlers still exist in daemon for backward compatibility but can be removed

## File-Based IPC Removed

- Operation request files (build_request.json, deploy_request.json, etc.) - REMOVED
- Device request/response files (device_list_*.json, etc.) - REMOVED
- File polling in daemon main loop - REMOVED
- Signal files KEPT (shutdown.signal, cancel_*.signal, clear_stale_locks.signal) - simple and effective
- Connection files KEPT (connect_*.json, heartbeat_*.json) - lightweight heartbeat mechanism

## WebSocket Async Handling Pattern (Iteration 9 Fix)

The WebSocket Serial Monitor endpoint uses a concurrent task pattern to handle messages without blocking:

```python
async def websocket_serial_monitor_api(websocket, context):
    # Message queue for decoupling receive from processing
    message_queue = asyncio.Queue()

    # Task 1: Receive messages from client and queue them
    async def message_receiver():
        while running:
            data = await websocket.receive_text()
            msg = json.loads(data)
            await message_queue.put(msg)

    # Task 2: Process messages from queue
    async def message_processor():
        while running:
            msg = await message_queue.get()
            if msg["type"] == "attach":
                # Process in thread pool (doesn't block)
                response = await loop.run_in_executor(None, processor.handle_attach, ...)
                # Send response immediately (receiver is still running!)
                await websocket.send_json({"type": "attached", ...})

    # Task 3: Push data to client (runs independently)
    async def data_pusher():
        while True:
            if attached and port:
                response = await loop.run_in_executor(None, processor.handle_poll, ...)
                if response.lines:
                    await websocket.send_json({"type": "data", ...})
            await asyncio.sleep(0.1)

    # Start data pusher as background task
    pusher_task = asyncio.create_task(data_pusher())

    # Run receiver and processor concurrently
    await asyncio.gather(message_receiver(), message_processor())
```

**Key improvements** (vs iteration 8 implementation):
- Receiver and processor run concurrently → responses sent immediately
- Data pusher runs as background task → doesn't block gather()
- Message queue decouples receiving from processing → no deadlocks
- Thread pool executor used for sync operations → doesn't block async loop
- Proper cleanup with task cancellation → no resource leaks
