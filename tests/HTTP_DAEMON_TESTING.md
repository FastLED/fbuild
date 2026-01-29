# HTTP Daemon Testing Guide

This document describes the comprehensive test suite for the HTTP/WebSocket daemon migration.

## Overview

The fbuild daemon has been migrated from file-based IPC to HTTP/WebSocket communication. This test suite verifies that all functionality works correctly with the new architecture.

## Test Environment Configuration

### Port Configuration

All tests use a custom port (9176) to avoid conflicts with production daemons:

```bash
export FBUILD_DEV_MODE=1
export FBUILD_DAEMON_PORT=9176
```

On Windows (CMD):
```cmd
set FBUILD_DEV_MODE=1
set FBUILD_DAEMON_PORT=9176
```

On Windows (PowerShell):
```powershell
$env:FBUILD_DEV_MODE = "1"
$env:FBUILD_DAEMON_PORT = "9176"
```

### Test Categories

1. **Unit Tests** (`tests/unit/daemon/test_http_client.py`)
   - Port discovery (environment variable, port file, dev mode)
   - HTTP client configuration
   - Request/response serialization
   - Error handling
   - Daemon availability checks

2. **Integration Tests** (`tests/integration/test_http_endpoints.py`)
   - All HTTP endpoints (build, deploy, monitor, install-deps)
   - Device management (list, status, lease, release, preempt)
   - Lock management (status, clear)
   - Daemon status and shutdown
   - Concurrent request handling

3. **Hardware Tests** (`tests/hardware/test_esp32s3_http_daemon.py`)
   - ESP32-S3 specific build tests
   - ESP32-S3 deploy tests (requires hardware)
   - Device lock management
   - End-to-end workflows
   - Performance benchmarks
   - Port configuration verification

4. **WebSocket Tests** (`tests/integration/test_websocket_serial_monitor_full.py`)
   - WebSocket connection lifecycle
   - Attach/detach operations
   - Data streaming
   - Write operations
   - Preemption handling
   - Concurrent connections
   - Error scenarios
   - Reconnection handling

## Running Tests

### Prerequisites

```bash
# Install test dependencies
uv run --group test pip install pytest pytest-asyncio websockets
```

### Run All Unit Tests

```bash
# Fast unit tests (parallel execution)
uv run --group test pytest tests/unit/daemon/test_http_client.py -v -n auto
```

### Run Integration Tests

```bash
# Integration tests (requires daemon)
uv run --group test pytest tests/integration/test_http_endpoints.py -v -s
```

### Run WebSocket Tests

```bash
# WebSocket serial monitor tests
uv run --group test pytest tests/integration/test_websocket_serial_monitor_full.py -v -s
```

### Run ESP32-S3 Hardware Tests

```bash
# Requires ESP32-S3 hardware connected
uv run --group test pytest tests/hardware/test_esp32s3_http_daemon.py -v -s -m esp32s3
```

### Run Complete Test Suite

```bash
# All tests (unit + integration + hardware)
export FBUILD_DAEMON_PORT=9176
./test --full
```

## Test Coverage

### Unit Tests (tests/unit/daemon/test_http_client.py)

**Port Discovery:**
- ✅ Port from `FBUILD_DAEMON_PORT` environment variable
- ✅ Port from port file
- ✅ Port from dev mode
- ✅ Port priority: env var > port file > dev mode
- ✅ Invalid port handling

**URL Generation:**
- ✅ Base URL with custom port
- ✅ Endpoint URLs with path normalization
- ✅ Empty path handling

**HTTP Client:**
- ✅ Default timeout configuration
- ✅ Custom timeout configuration
- ✅ Redirect following

**Daemon Availability:**
- ✅ Health check success
- ✅ Connection error handling
- ✅ Timeout handling
- ✅ HTTP error handling
- ✅ Wait for daemon with polling

**Serialization:**
- ✅ Request serialization
- ✅ Response deserialization
- ✅ Error handling for invalid objects

### Integration Tests (tests/integration/test_http_endpoints.py)

**Health Endpoints:**
- ✅ `/health` endpoint
- ✅ `/` root endpoint
- ✅ `/api/daemon/info` endpoint

**Device Endpoints:**
- ✅ List devices
- ✅ Get device status
- ✅ Acquire/release device lease
- ✅ Preempt device

**Lock Endpoints:**
- ✅ Get lock status
- ✅ Clear stale locks

**Build Endpoint:**
- ✅ Build Arduino Uno project
- ✅ Build ESP32-C6 project
- ✅ Build nonexistent project (error handling)

**Install Dependencies:**
- ✅ Install dependencies for project

**Monitor Endpoint:**
- ✅ Monitor request handling

**Daemon Management:**
- ✅ Graceful shutdown

**Concurrency:**
- ✅ Concurrent health checks
- ✅ Concurrent device list requests

### Hardware Tests (tests/hardware/test_esp32s3_http_daemon.py)

**Build Tests:**
- ✅ Standard build via HTTP
- ✅ Clean build via HTTP
- ✅ Serial compilation (jobs=1)
- ✅ Parallel compilation (jobs=4, 8)

**Deploy Tests:**
- ✅ Deploy to ESP32-S3 via HTTP (requires hardware)

**Device Lock Tests:**
- ✅ Device list includes ESP32-S3
- ✅ Device status retrieval

**End-to-End:**
- ✅ Build-deploy workflow

**Performance:**
- ✅ Build performance benchmarks

**Reliability:**
- ✅ Multiple sequential builds
- ✅ Builds with different job counts

**Configuration:**
- ✅ Custom port verification
- ✅ Environment variable handling

### WebSocket Tests (tests/integration/test_websocket_serial_monitor_full.py)

**Basic Operations:**
- ✅ WebSocket connection
- ✅ Attach/detach cycle
- ✅ Ping-pong heartbeat

**Data Streaming:**
- ✅ Receive serial data
- ✅ Data message format validation

**Write Operations:**
- ✅ Write to serial port
- ✅ Write acknowledgment

**Preemption:**
- ✅ Deploy preempts monitor session

**Concurrent Connections:**
- ✅ Multiple concurrent WebSocket connections

**Error Handling:**
- ✅ Attach to invalid port
- ✅ Invalid message type
- ✅ Malformed JSON

**Reconnection:**
- ✅ Reconnect after disconnect
- ✅ Same client ID reconnection

## Expected Results

### Unit Tests
- **Expected duration:** < 5 seconds
- **Expected pass rate:** 100%
- **Dependencies:** None (uses mocks)

### Integration Tests
- **Expected duration:** 1-3 minutes
- **Expected pass rate:** 95%+ (some tests may skip if dependencies not available)
- **Dependencies:** Daemon running, test projects exist

### Hardware Tests
- **Expected duration:** 5-10 minutes (depends on build cache)
- **Expected pass rate:** 90%+ (some tests require specific hardware)
- **Dependencies:** ESP32-S3 connected, daemon running

### WebSocket Tests
- **Expected duration:** 1-2 minutes
- **Expected pass rate:** 95%+ (some tests may skip if no devices)
- **Dependencies:** Daemon with WebSocket support, serial device (optional)

## Debugging

### Enable Verbose Output

```bash
# Run with verbose output and no capture
pytest -v -s tests/...
```

### Check Daemon Logs

```bash
# View daemon logs in dev mode
tail -f .fbuild/daemon_dev/daemon.log
```

### Check Port Configuration

```bash
# Verify port configuration
python -c "from fbuild.daemon.client.http_utils import get_daemon_port; print(get_daemon_port())"
```

### Test Daemon HTTP Server Manually

```bash
# Health check
curl http://127.0.0.1:9176/health

# Daemon info
curl http://127.0.0.1:9176/api/daemon/info

# OpenAPI docs (in browser)
open http://127.0.0.1:9176/docs
```

## Continuous Integration

### GitHub Actions

```yaml
- name: Run HTTP Daemon Tests
  env:
    FBUILD_DEV_MODE: "1"
    FBUILD_DAEMON_PORT: "9176"
  run: |
    # Unit tests (fast)
    uv run --group test pytest tests/unit/daemon/test_http_client.py -v -n auto

    # Integration tests
    uv run --group test pytest tests/integration/test_http_endpoints.py -v

    # WebSocket tests
    uv run --group test pytest tests/integration/test_websocket_serial_monitor_full.py -v
```

### Local Pre-commit Hook

```bash
#!/bin/bash
# .git/hooks/pre-commit

export FBUILD_DEV_MODE=1
export FBUILD_DAEMON_PORT=9176

# Run HTTP daemon unit tests
uv run --group test pytest tests/unit/daemon/test_http_client.py -v -n auto || exit 1

echo "HTTP daemon tests passed!"
```

## Known Issues

### Port Conflicts

If tests fail with "Address already in use":
- Another daemon may be running on port 9176
- Stop all daemons: `fbuild daemon stop`
- Verify port is free: `netstat -an | grep 9176` (Linux/Mac) or `netstat -an | findstr 9176` (Windows)

### WebSocket Connection Errors

If WebSocket tests fail:
- Ensure daemon is running with WebSocket support (v1.3.28+)
- Check firewall settings
- Verify websockets library is installed: `pip install websockets`

### Hardware Tests Skipped

Hardware tests will skip if:
- No ESP32-S3 device connected
- Device not detected by system
- Wrong COM port/device path

## Migration Verification Checklist

- [x] Port configuration supports `FBUILD_DAEMON_PORT` environment variable
- [x] All HTTP endpoints tested
- [x] WebSocket endpoints tested
- [x] Device management via HTTP tested
- [x] Lock management via HTTP tested
- [x] Build operations via HTTP tested
- [x] Deploy operations via HTTP tested
- [x] Serial monitor via WebSocket tested
- [x] Concurrent request handling tested
- [x] Error scenarios tested
- [x] ESP32-S3 specific tests created
- [x] Performance benchmarks included
- [x] Documentation complete

## Conclusion

This comprehensive test suite verifies that the HTTP/WebSocket daemon migration maintains full functionality while improving performance and reliability. All critical paths are covered with unit, integration, and hardware tests.

For issues or questions, see:
- `CLAUDE.md` for architecture details
- `docs/parameter_flow.md` for implementation patterns
- GitHub issues for bug reports
