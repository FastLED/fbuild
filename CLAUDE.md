# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

fbuild is a PlatformIO-compatible embedded development tool providing build, deploy, and monitor functionality for Arduino/ESP32 platforms. It uses URL-based package management and a daemon for cross-process coordination.

**Current Version:** v1.3.35 (update in `src/fbuild/__init__.py`, `pyproject.toml`, and this file)

## Development Commands

```bash
# Install in development mode
pip install -e .

# IMPORTANT: Enable dev mode to isolate from production
export FBUILD_DEV_MODE=1  # Linux/macOS
set FBUILD_DEV_MODE=1     # Windows CMD

# Run unit tests (fast, parallel)
uv run --group test pytest -n auto tests/unit -v

# Run single test file
uv run --group test pytest tests/unit/test_foo.py -v

# Run all tests including integration (slow)
./test --full

# Lint and format
./lint  # Runs: ruff, black, isort, pyright, flake8

# Build a test project
fbuild build tests/uno -e uno
fbuild build tests/esp32c6 -e esp32c6 -v  # verbose

# Parallel compilation (default: uses all CPU cores)
fbuild build tests/esp32c6 -e esp32c6     # automatic parallel
fbuild build tests/uno -e uno --jobs 4    # use 4 workers
fbuild build tests/uno -e uno --jobs 1    # serial (debugging)

# Deploy and monitor
fbuild deploy tests/esp32c6 --monitor
```

## Architecture

See `docs/architecture.dot` for Graphviz diagram. Render with: `dot -Tpng docs/architecture.dot -o architecture.png`

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                               CLI LAYER                                     │
│  cli.py ──► build/deploy/monitor commands ──► daemon/client.py (IPC)       │
└─────────────────────────────────────────────────────────────────────────────┘
                                      │
                                      ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                      DAEMON LAYER (Background Process)                      │
│  daemon/daemon.py                                                           │
│  ├── lock_manager.py (ResourceLockManager) ◄── Memory-based locks only!    │
│  ├── device_manager.py ──► device_discovery.py, shared_serial.py           │
│  └── processors/                                                            │
│      ├── build_processor.py ────► BUILD LAYER                               │
│      ├── deploy_processor.py ───► DEPLOY LAYER                              │
│      └── monitor_processor.py ──► monitor.py                                │
└─────────────────────────────────────────────────────────────────────────────┘
           │                    │                    │
           ▼                    ▼                    ▼
┌─────────────────┐  ┌─────────────────┐  ┌───────────────────────────────────┐
│  CONFIG LAYER   │  │ PACKAGES LAYER  │  │          BUILD LAYER              │
│                 │  │                 │  │                                   │
│ ini_parser.py   │  │ cache.py        │  │ Platform Orchestrators:           │
│ (PlatformIO     │  │     │           │  │ orchestrator.py (interface)       │
│  Config)        │  │     ▼           │  │ ├── orchestrator_avr.py           │
│     │           │  │ downloader.py   │  │ ├── orchestrator_esp32.py         │
│     ▼           │  │     │           │  │ ├── orchestrator_rp2040.py        │
│ board_config.py │  │     ▼           │  │ ├── orchestrator_stm32.py         │
│     │           │  │ Toolchains:     │  │ └── orchestrator_teensy.py        │
│     ▼           │  │ toolchain.py    │  │            │                      │
│ board_loader.py │  │ toolchain_      │  │            ▼                      │
│     │           │  │   esp32.py      │  │ Compilation:                      │
│     ▼           │  │ toolchain_      │  │ source_scanner.py                 │
│ mcu_specs.py    │  │   rp2040.py ... │  │ compiler.py / configurable_       │
│                 │  │     │           │  │ flag_builder.py                   │
│                 │  │     ▼           │  │            │                      │
│                 │  │ Frameworks:     │  │            ▼                      │
│                 │  │ arduino_core.py │  │ Linking:                          │
│                 │  │ framework_      │  │ linker.py / configurable_linker   │
│                 │  │   esp32.py ...  │  │ archive_creator.py                │
│                 │  │     │           │  │ binary_generator.py               │
│                 │  │     ▼           │  │            │                      │
│                 │  │ Libraries:      │  │            ▼                      │
│                 │  │ library_        │  │     firmware.hex/.bin             │
│                 │  │   manager.py    │  │                                   │
│                 │  │ library_        │  │ build_state.py (incremental)      │
│                 │  │   compiler.py   │  │                                   │
└─────────────────┘  └─────────────────┘  └───────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────────────┐
│                              DEPLOY LAYER                                   │
│  deployer.py (IDeployer)  deployer_esp32.py   monitor.py   qemu_runner.py  │
│         │                       │                 │              │          │
│         ▼                       ▼                 ▼              ▼          │
│     [avrdude]              [esptool]         [pyserial]      [Docker]       │
└─────────────────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────────────┐
│                              LEDGER LAYER                                   │
│  ledger/board_ledger.py          daemon/firmware_ledger.py                  │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Key Data Flows:**
1. **Build**: CLI → HTTP → Daemon → Build Processor → Orchestrator → Compiler → Linker → firmware
2. **Deploy**: CLI → HTTP → Daemon → Deploy Processor → Deployer (esptool/avrdude) → Device
3. **Packages**: Orchestrator → Cache → Downloader → fingerprint → extracted packages

### HTTP/WebSocket Architecture (as of v1.3.28)

**The daemon now uses FastAPI for all client-daemon communication**, replacing the previous file-based IPC system. This provides:

- **Better performance**: No file system polling, instant request handling
- **Real-time updates**: WebSocket streaming for build output, monitor sessions, logs
- **Better error handling**: HTTP status codes, structured error responses
- **Standard tooling**: OpenAPI docs, type validation via Pydantic
- **Cleaner codebase**: Removed ~500 lines of file polling logic

#### Communication Endpoints

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

**WebSocket Serial Monitor API Protocol** (`/ws/serial-monitor`):
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

**Client Utilities**:
- `src/fbuild/daemon/client/http_utils.py` - HTTP client helpers, port discovery
- `src/fbuild/daemon/client/requests_http.py` - HTTP request functions
- `src/fbuild/daemon/client/devices_http.py` - Device management via HTTP
- `src/fbuild/daemon/client/locks_http.py` - Lock management via HTTP
- `src/fbuild/daemon/client/websocket_client.py` - WebSocket client helpers
- `src/fbuild/api/serial_monitor.py` - WebSocket-based SerialMonitor API (v2)
- `src/fbuild/api/serial_monitor_file.py` - File-based SerialMonitor API (deprecated, v1)

**Migration Status**:
- ✅ Build, deploy, monitor, install-deps operations → HTTP
- ✅ Device management (list, status, lease, release, preempt) → HTTP
- ✅ Lock management (status, clear) → HTTP
- ✅ Daemon status and shutdown → HTTP
- ✅ Real-time status updates → WebSocket
- ✅ Serial monitor sessions → WebSocket
- ✅ Daemon log streaming → WebSocket
- ✅ Serial Monitor API (fbuild.api.SerialMonitor) → **Fully migrated to WebSocket** (as of iteration 9)
  - **Status**: ✅ Async handling fixed - attach/detach operations work correctly
  - **Implementation**: Uses concurrent message receiver, processor, and data pusher tasks
  - **Performance**: Real-time data streaming with <10ms latency (vs 100ms file-based polling)
  - **Note**: File-based handlers still exist in daemon for backward compatibility but can be removed

**File-Based IPC Removed**:
- ❌ Operation request files (build_request.json, deploy_request.json, etc.)
- ❌ Device request/response files (device_list_*.json, etc.)
- ❌ File polling in daemon main loop
- ✅ Signal files KEPT (shutdown.signal, cancel_*.signal, clear_stale_locks.signal) - simple and effective
- ✅ Connection files KEPT (connect_*.json, heartbeat_*.json) - lightweight heartbeat mechanism

**WebSocket Async Handling Pattern** (Iteration 9 Fix):

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
- ✅ Receiver and processor run concurrently → responses sent immediately
- ✅ Data pusher runs as background task → doesn't block gather()
- ✅ Message queue decouples receiving from processing → no deadlocks
- ✅ Thread pool executor used for sync operations → doesn't block async loop
- ✅ Proper cleanup with task cancellation → no resource leaks

## Critical Constraints

### Locking Strategy: Memory-Based Daemon Locks Only

**Do NOT use file-based locks** (`fcntl`, `msvcrt`, `.lock` files). All cross-process synchronization goes through the daemon's `ResourceLockManager`. Use `threading.Lock` for in-process synchronization only.

### Daemon Lifecycle Management

**NEVER kill all Python processes** when working with fbuild. The daemon is shared across projects and killing it blindly will:
- Interrupt other users' builds in progress
- Kill Claude Code's own Python process (causing session termination)
- Leave resources in an inconsistent state

**To stop the daemon gracefully, use one of these methods:**

1. **HTTP API (preferred):**
   ```bash
   curl -X POST http://127.0.0.1:8765/api/daemon/shutdown
   ```

2. **Kill specific daemon PID:**
   ```bash
   # Get daemon PID first
   curl -s http://127.0.0.1:8765/api/daemon/info | python -c "import sys,json; print(json.load(sys.stdin)['pid'])"
   # Then kill only that process
   taskkill //PID <pid> //F  # Windows
   kill <pid>                 # Linux/macOS
   ```

3. **Signal file (fallback):**
   ```bash
   touch ~/.fbuild/daemon/shutdown.signal
   ```

**NEVER run** commands like `pkill python`, `taskkill /IM python.exe /F`, or any command that kills all Python processes.

### Development Mode

Always set `FBUILD_DEV_MODE=1` when developing. This isolates:
- Daemon files → `.fbuild/daemon_dev/` (instead of `~/.fbuild/daemon/`)
- Cache files → `.fbuild/cache_dev/` (instead of `.fbuild/cache/`)

### Platform Requirements

- Python 3.10+
- Windows requires git-bash for shell scripts
- Type hints required for all functions
- Line length: 200 chars (configured in pyproject.toml)

### Thread-Safe Output System

**All output goes through `src/fbuild/output.py` which uses `contextvars` for thread safety.**

The output system was refactored to use Python's `contextvars` instead of module-level globals. This ensures concurrent builds don't interfere with each other's:
- **Timestamps** (`start_time`) - Each build has isolated elapsed time tracking
- **Output files** (`output_file`) - Each build writes to its own output file
- **Verbose flags** (`verbose`) - Each build has independent verbosity settings
- **Output streams** (`output_stream`) - Isolated stream handling

**Key features:**
- **Context survives module reloads** - Unlike globals, contextvars are stored in the interpreter, not the module
- **Automatic thread isolation** - Each thread gets a copy of the parent context
- **Explicit isolation in processors** - Build processor uses `contextvars.copy_context()` for guaranteed isolation

**Implementation pattern:**
```python
# In build_processor.py
import contextvars

def execute_operation(self, request, context):
    # Run build in isolated context
    ctx = contextvars.copy_context()
    return ctx.run(self._execute_operation_isolated, request, context)
```

**Testing:**
- `tests/unit/test_concurrent_output_bug.py` - Demonstrates the original bug and verifies the fix
- Tests use `run_in_isolated_context()` helper to ensure proper context isolation
- Mark concurrent safety tests with `@pytest.mark.concurrent_safety`

**⚠️ DEPRECATED:** Module-level globals (`_start_time`, `_output_stream`, `_verbose`, `_output_file`) are kept for backward compatibility but will be removed in a future version. Always use the context API (`get_context()`, `set_output_file()`, etc.).

## Test Organization

- `tests/unit/` - Fast unit tests, run in parallel with `-n auto`
- `tests/integration/` - Slow integration tests (use `--full` flag)
- `tests/{uno,esp32c6,esp32dev,...}/` - Hardware test projects with platformio.ini

Markers: `@pytest.mark.integration`, `@pytest.mark.concurrent_safety`, `@pytest.mark.hardware`

## Configuration Format

Uses standard platformio.ini with extensions:
- `extends = env:base` - Environment inheritance (multi-level supported)
- `${env:parent.key}` - Variable substitution
- `board_build.*` / `board_upload.*` - Board overrides
- `symlink://./path` - Local library symlinks (auto-converted to copies on Windows)

## Parallel Compilation

fbuild uses parallel compilation to speed up builds by compiling multiple source files simultaneously:

- **Configurable**: Use `--jobs N` or `-j N` flag to control worker count
- **Serial Mode**: Use `--jobs 1` for serial compilation (debugging)
- **Implementation**: Daemon's `CompilationJobQueue` manages worker thread pool

**Examples:**
```bash
fbuild build tests/esp32c6 -e esp32c6 --jobs 4  # Use 4 workers
fbuild build tests/uno -e uno --jobs 2          # Use 2 workers
fbuild build tests/esp32c6 -e esp32c6 --jobs 1  # Serial (debugging)
```

### Platform Support

**✅ Validated Platforms** (parallel compilation tested and working):
- **AVR** (Arduino Uno, etc.) - Integration tests: `tests/integration/test_parallel_uno.py`
- **Teensy** (Teensy 4.1, etc.) - Integration tests: `tests/integration/test_parallel_teensy.py`
- **ESP32** (ESP32dev, ESP32C6, etc.) - Integration tests: `tests/integration/test_parallel_esp32.py`

**❌ Known Platform Issues**:
- **RP2040** (Raspberry Pi Pico) - Pre-existing platform bug: missing ArduinoCore-API dependency (affects all build modes)
- **STM32** (BluePill, etc.) - Pre-existing platform bug: missing include paths (affects all build modes)

These platform issues are NOT related to parallel compilation and affect serial builds as well.

### Known Issues

**Auto Mode (jobs=None) Bug**: Using `--jobs` without a value (auto mode) currently fails due to a module reload bug in the daemon.

**Workaround**: Always specify an explicit `--jobs N` value (e.g., `--jobs 4`, `--jobs 2`).

**Future Fix**: Pass compilation queue directly from daemon context instead of using global accessor.

### Performance

Parallel compilation provides significant speedups on multi-core systems:
- **Teensy 4.1**: 11.8x faster (991.9s serial → 83.5s with --jobs 2)
- **AVR Uno**: ~2-3x faster on typical projects
- **ESP32**: Modest improvements (4-10% faster due to smaller core size)

Actual speedup depends on:
- Number of CPU cores
- Project size (more source files = better parallelization)
- I/O performance (Windows file locking can reduce gains)

## Architecture Patterns and Protocols

### SerializableMessage Protocol

All daemon messages implement the `SerializableMessage` protocol for type-safe serialization:

**File**: `src/fbuild/daemon/message_protocol.py`

```python
@runtime_checkable
class SerializableMessage(Protocol):
    """Protocol for messages that can be serialized to/from dictionaries."""

    def to_dict(self) -> dict[str, Any]:
        """Convert this message to a dictionary for JSON serialization."""
        ...

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "SerializableMessage":
        """Create a message instance from a dictionary."""
        ...
```

**Key features**:
- Automatic enum serialization/deserialization
- Support for nested SerializableMessage objects
- Proper handling of Optional types
- Respects field defaults

**Usage**:
```python
@dataclass
class BuildRequest:
    project_dir: str
    jobs: int | None = None

    def to_dict(self) -> dict[str, Any]:
        return serialize_dataclass(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "BuildRequest":
        return deserialize_dataclass(cls, data)
```

### PlatformBuildMethod Protocol

All platform-specific build methods follow the `PlatformBuildMethod` protocol signature:

**File**: `src/fbuild/build/orchestrator.py`

```python
@runtime_checkable
class PlatformBuildMethod(Protocol):
    """Protocol defining the expected signature for internal _build_XXX() methods."""

    def __call__(
        self,
        project_path: Path,
        env_name: str,
        target: str,
        verbose: bool,
        clean: bool,
        jobs: int | None = None,
    ) -> BuildResult:
        """Execute platform-specific build."""
        ...
```

**Purpose**: Ensures all platform orchestrators (AVR, ESP32, Teensy, RP2040, STM32) accept the same parameters, enabling consistent parameter passing.

### managed_compilation_queue() Context Manager

The `managed_compilation_queue()` context manager handles compilation queue lifecycle and resource cleanup:

**File**: `src/fbuild/build/orchestrator.py`

```python
@contextlib.contextmanager
def managed_compilation_queue(jobs: int | None, verbose: bool = False):
    """Context manager for safely managing compilation queue lifecycle.

    Args:
        jobs: Number of parallel compilation jobs
              - None: Use CPU count (daemon's shared queue)
              - 1: Serial mode (no queue)
              - N: Custom worker count (temporary queue)
        verbose: Whether to log queue selection and lifecycle events

    Yields:
        Optional[CompilationJobQueue]: The queue to use, or None for serial mode
    """
    queue, should_cleanup = get_compilation_queue_for_build(jobs, verbose)
    try:
        yield queue
    finally:
        if should_cleanup and queue:
            queue.shutdown_and_wait()  # Automatic cleanup
```

**Usage in orchestrators**:
```python
def build(self, project_dir: Path, ..., jobs: int | None = None) -> BuildResult:
    with managed_compilation_queue(jobs, verbose=self.verbose) as queue:
        # Queue is available throughout build
        # Automatically cleaned up on exit (even if exception occurs)
        return self._build_internal(...)
```

**Queue selection strategy**:
1. `jobs=1` → Serial mode (returns None)
2. `jobs=None` or `jobs=cpu_count()` → Daemon's shared queue (no cleanup)
3. `jobs=N` (custom) → Temporary queue with N workers (requires cleanup)

## Daemon Availability

The fbuild daemon is **always running** during operations:

- **Auto-Start**: CLI automatically starts daemon if not running
- **Shared Queue**: Daemon maintains shared compilation queue with CPU-count workers
- **Lifecycle**: Daemon auto-evicts after 4 seconds of inactivity
- **Port Configuration**: Production uses port 8765, dev mode uses port 8865 (prod + 100)

**Serial Mode**: Only occurs when user explicitly requests `--jobs 1` for debugging.
This is NOT a fallback - it's an intentional design choice.

### Daemon Spawn Race Condition Handling

**Status**: ✅ FIXED in v1.3.31 (2026-01-29)

fbuild handles concurrent daemon spawn attempts safely using a defense-in-depth approach:

**Problem**: On Windows, `subprocess.Popen()` can return a PID before the process fully initializes. If the process crashes during startup, the client waits 10s for a PID that will never write the PID file, causing spurious errors even when a concurrent spawn succeeds.

**Solution**:
1. **Permissive PID Acceptance** - `wait_for_pid_file()` accepts any alive daemon PID, not just the expected one
2. **Exponential Backoff Retry** - Up to 3 spawn attempts with delays (0s, 500ms, 2s)
3. **HTTP Health Check Fallback** - If PID file wait fails, check if daemon HTTP is available
4. **Append Mode Logging** - Spawn log preserves all attempts for debugging
5. **Atomic Singleton Lock** - Only one process can spawn daemon at a time

**Test Coverage**: `tests/unit/daemon/test_daemon_spawn_race.py` and `tests/stress_test_daemon_spawn.py`

**Validation**: 10/10 concurrent spawns succeeded with zero spurious failures in stress testing.

## Parameter Flow

See `docs/parameter_flow.md` for comprehensive documentation on how parameters flow through the system from CLI to orchestrator. This includes:

- Architecture overview with layer-by-layer diagrams
- Complete example using the `jobs` parameter
- Context manager pattern for resource management
- Step-by-step guide for adding new parameters
- Testing strategies (unit, integration, system)
- Best practices and common pitfalls

**Quick reference**:
```
CLI --jobs N → BuildArgs(jobs=N) → BuildRequest(jobs=N) →
JSON serialization → Daemon → BuildProcessor →
Orchestrator.build(jobs=N) → managed_compilation_queue(jobs=N)
```

## Linting and Code Quality

### Custom Linting Checks

fbuild includes custom linting plugins to enforce architectural patterns:

**Run all lints**:
```bash
./lint  # Runs: ruff, black, isort, pyright, custom checks
```

**Custom checks**:
1. **Orchestrator Signature Validation** (`scripts/check_orchestrator_signatures.py`)
   - Ensures all platform orchestrators implement `IBuildOrchestrator` interface
   - Validates internal build methods follow `PlatformBuildMethod` protocol
   - Checks for required parameters (including `jobs`)

2. **Message Serialization Checker** (`scripts/check_message_serialization.py`)
   - Verifies all daemon messages implement `SerializableMessage` protocol
   - Checks for proper enum handling in serialization

3. **KeyboardInterrupt Checker** (`scripts/check_keyboard_interrupt.py`)
   - Validates that try-except blocks properly handle KeyboardInterrupt
   - Ensures bare except clauses don't accidentally catch Ctrl+C
   - Implementation: `fbuild_lint/ruff_plugins/keyboard_interrupt_checker.py` (dev-only)

4. **Sys.Path Checker** (`scripts/check_sys_path.py`)
   - Detects improper sys.path.insert() usage outside test files
   - Prevents fragile import hacks in production code
   - Implementation: `fbuild_lint/ruff_plugins/sys_path_checker.py` (dev-only)

5. **Subprocess Safety Checker** (`scripts/check_subprocess_safety.py`)
   - Detects direct `subprocess.run()` / `subprocess.Popen()` calls
   - Enforces use of `safe_run()` / `safe_popen()` from `subprocess_utils.py`
   - Prevents ephemeral console windows on Windows
   - Error codes: SUB001-SUB005
   - Implementation: `fbuild_lint/ruff_plugins/subprocess_safety_checker.py` (dev-only)
   - See: `docs/subprocess_safety.md`

### Custom Linting Architecture

**Implementation:**
- Plugin implementations: `fbuild_lint/ruff_plugins/` (NOT distributed with package)
- Standalone runners: `scripts/check_*.py` (invoke plugins via AST parsing)
- All checks use AST analysis for zero runtime overhead
- Plugins are excluded from distributed packages to prevent global pollution

**Why Standalone Scripts?**
Previously, plugins were registered via flake8 entry points, which caused them to activate globally for all Python projects when fbuild was installed. Now, plugins are only invoked explicitly via standalone scripts during fbuild development, ensuring they don't affect other projects.

**Run signature validation**:
```bash
python scripts/check_orchestrator_signatures.py
```

**Expected output**:
```
Validating orchestrator signatures...

[orchestrator_avr] BuildOrchestratorAVR
  ✓ Inherits from IBuildOrchestrator
  ✓ Implements build() method
  ✓ build() signature matches IBuildOrchestrator
  ✓ Has platform build method: _build_avr
  ✓ _build_avr signature matches PlatformBuildMethod protocol

[orchestrator_esp32] OrchestratorESP32
  ✓ Inherits from IBuildOrchestrator
  ...

All orchestrators validated successfully.
```

## Best Practices for Adding Parameters

When adding new CLI parameters that need to flow through to orchestrators:

1. **Define in CLI**: Add to `BuildArgs` dataclass and argparse
2. **Add to Message**: Update `BuildRequest` in `messages.py`
3. **Update Client**: Pass parameter in `daemon/client.py`
4. **Extract in Processor**: Forward to orchestrator in `build_processor.py`
5. **Update Interface**: Add to `IBuildOrchestrator` in `orchestrator.py`
6. **Implement**: Add to all platform orchestrators
7. **Test**: Add integration tests in `test_parameter_flow.py`
8. **Validate**: Run `./lint` to verify signature compliance

See `docs/parameter_flow.md` for detailed examples and step-by-step instructions.

## Subprocess Safety

**ALWAYS use safe subprocess wrappers** to prevent console issues on Windows:

```python
# ❌ UNSAFE - Direct subprocess calls
result = subprocess.run(cmd, ...)
proc = subprocess.Popen(cmd, ...)

# ✅ SAFE - Use wrappers from subprocess_utils
from fbuild.subprocess_utils import safe_run, safe_popen

result = safe_run(cmd, ...)
proc = safe_popen(cmd, ...)
```

**CRITICAL: Use pythonw.exe for Python subprocess calls:**

```python
# ❌ UNSAFE - Uses python.exe (shows console window)
cmd = [sys.executable, "-m", "esptool", ...]

# ✅ SAFE - Uses pythonw.exe on Windows (no console window)
from fbuild.subprocess_utils import get_python_executable

cmd = [get_python_executable(), "-m", "esptool", ...]
```

**What the utilities provide:**
1. **get_python_executable()**: Returns `pythonw.exe` on Windows (no console), `sys.executable` elsewhere
2. **safe_run()/safe_popen()**: Apply `CREATE_NO_WINDOW` flag and auto-redirect stdin
3. **Prevent keystroke loss**: Auto-redirects stdin to `subprocess.DEVNULL` to prevent child processes from stealing keyboard input

**stdin Auto-Redirect:**
- By default, stdin is redirected to `subprocess.DEVNULL`
- Prevents child processes from inheriting the parent's console input handle
- Fixes issues where background processes steal keystrokes from the terminal
- Can be overridden with explicit `stdin=` parameter if needed (e.g., for interactive processes)

**Enforcement**: The `SUB` flake8 plugin (run via `./lint`) detects unsafe subprocess calls.

**Details**: See `docs/subprocess_safety.md` for complete documentation and `INVESTIGATION.md` for the technical analysis of the keystroke loss issue.
