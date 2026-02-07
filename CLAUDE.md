# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

fbuild is a PlatformIO-compatible embedded development tool providing build, deploy, and monitor functionality for Arduino/ESP32 platforms. It uses URL-based package management and a daemon for cross-process coordination.

**Current Version:** v1.4.4 (update in `src/fbuild/__init__.py`, `pyproject.toml`, and this file)

**Recent Changes (v1.4.4):**
- Added parallel package installation pipeline with Docker pull-style TUI
- Three-stage pipeline: Download (4 workers) → Unpack (2 workers) → Install (2 workers)
- DAG-based dependency scheduler respects platform → toolchain → framework → library ordering
- Rich-based progress display with live-updating progress bars, spinners, and status lines
- Download retry with exponential backoff, extraction retry for Windows AV, Ctrl-C cleanup

**Previous Changes (v1.4.3):**
- Fixed CTRL-C deadlock in HTTP client - client now responds to keyboard interrupt within 0.5 seconds
- Replaced blocking `httpx.Client.post()` with interruptible wrapper using background threads
- See `docs/fix_ctrl_c_deadlock.md` for technical details

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

# Build profiles (default: release with LTO)
fbuild build tests/uno -e uno              # release build (default, LTO enabled)
fbuild build tests/uno -e uno --release    # explicit release build
fbuild build tests/uno -e uno --quick      # quick build (no LTO, faster compile)

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

### No Default Arguments Policy

**Default arguments are forbidden** in function and method signatures. All arguments must be explicitly specified at call sites.

**Allowed exceptions:**
1. **`None` as default**: Parameters can have `None` as a default when the parameter is truly optional or for testing scenarios where a particular parameter is not needed
2. **Public API objects in `__init__`**: Classes exposed in `fbuild/__init__.py` may have default arguments for backwards compatibility

**Why:**
- Explicit is better than implicit
- Prevents hidden coupling between components
- Makes code more testable and refactorable
- BuildContext consolidation (v1.3.37+) follows this pattern - all configuration is explicit

**Example:**
```python
# BAD - default arguments hide dependencies
def compile(source: Path, flags: List[str] = [], verbose: bool = False):
    ...

# GOOD - all arguments explicit, None allowed for optional
def compile(source: Path, flags: List[str], verbose: bool):
    ...

# GOOD - None default for truly optional parameter
def compile(source: Path, flags: List[str], output: Path | None = None):
    ...
```

### Type-Safe Configuration with Dataclasses

**Use @dataclass structures instead of dict.get() for configuration objects.** This provides type safety, IDE autocomplete, and validation.

**Why:**
- Type safety: IDE autocomplete and compile-time type checking
- Validation: Errors caught at load time, not runtime
- Clarity: Explicit structure vs implicit dict keys
- Maintainability: Easier refactoring with strong types
- Self-documenting: Dataclass fields serve as documentation

**Pattern:**

```python
# BAD - dict-based configuration with runtime errors
config = load_config("teensy41")
mcu = config.get("mcu", "")  # What if key is misspelled?
f_cpu = config.get("f_cpu", "")  # What's the correct default?
variant = config.get("variant", "")  # No IDE autocomplete

# GOOD - dataclass-based configuration with type safety
@dataclass(frozen=True)
class BoardConfigModel:
    """Type-safe board configuration."""
    name: str
    mcu: str
    f_cpu: str = "16000000L"
    variant: str = ""

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "BoardConfigModel":
        """Parse and validate configuration from dict."""
        try:
            return cls(
                name=data["name"],  # Required - will raise if missing
                mcu=data["mcu"],
                f_cpu=data.get("f_cpu", "16000000L"),  # Optional with default
                variant=data.get("variant", ""),
            )
        except KeyError as e:
            raise ValueError(f"Missing required field: {e}")

# Usage - type-safe access with IDE support
config = load_config("teensy41")  # Returns BoardConfigModel
mcu = config.mcu  # IDE knows this is a str
f_cpu = config.f_cpu  # Autocomplete works
variant = config.variant  # Typos caught by type checker
```

**Implementation Guidelines:**

1. **Define dataclass models in dedicated files**: `src/fbuild/platform_configs/board_config_model.py`
2. **Use `frozen=True` for immutable configs**: Prevents accidental modification
3. **Provide `from_dict()` class method**: Parses JSON data with validation
4. **Validate required fields**: Raise `ValueError` with clear error messages
5. **Use nested dataclasses for complex structures**: e.g., `CompilerFlags`, `BuildProfile`
6. **Support backward compatibility when needed**: Accept both dataclass and dict in transitions

**Example - Nested Dataclasses:**

```python
@dataclass(frozen=True)
class CompilerFlags:
    """Compiler flag configuration."""
    common: List[str] = field(default_factory=list)
    c: List[str] = field(default_factory=list)
    cxx: List[str] = field(default_factory=list)

@dataclass(frozen=True)
class BoardConfigModel:
    """Type-safe board configuration."""
    name: str
    mcu: str
    compiler_flags: CompilerFlags = field(default_factory=CompilerFlags)

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "BoardConfigModel":
        flags_data = data.get("compiler_flags", {})
        compiler_flags = CompilerFlags(
            common=flags_data.get("common", []),
            c=flags_data.get("c", []),
            cxx=flags_data.get("cxx", []),
        )
        return cls(
            name=data["name"],
            mcu=data["mcu"],
            compiler_flags=compiler_flags,
        )

# Usage - deeply nested type-safe access
config = load_config("teensy41")
common_flags = config.compiler_flags.common  # Type: List[str]
c_flags = config.compiler_flags.c  # IDE autocomplete works
```

**See:** `src/fbuild/platform_configs/board_config_model.py` for the full implementation example.

### Development Mode

Always set `FBUILD_DEV_MODE=1` when developing. This isolates:
- Daemon files → `~/.fbuild/daemon_dev/` (instead of `~/.fbuild/daemon/`)
- Cache files → `~/.fbuild/cache_dev/` (isolated from `~/.fbuild/cache/`)
- Port → 8865 (instead of 8765)

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

## Parallel Package Pipeline

The parallel package pipeline (`src/fbuild/packages/pipeline/`) provides concurrent package installation with a Docker pull-style TUI display.

### Architecture

```
src/fbuild/packages/pipeline/
├── __init__.py              # Public API: ParallelInstaller
├── models.py                # PackageTask, TaskPhase, PipelineResult dataclasses
├── scheduler.py             # DAG-based dependency scheduler with cycle detection
├── pools.py                 # Static thread pools: DownloadPool, UnpackPool, InstallPool
├── pipeline.py              # Pipeline orchestrator connecting pools + scheduler
├── progress_display.py      # Rich-based Docker pull-style TUI renderer
├── callbacks.py             # ProgressCallback protocol + NullCallback
└── adapters.py              # Platform-specific task graph builders (AVR, etc.)
```

### Thread Pool Design

| Pool | Resource | Default Workers | Purpose |
|------|----------|-----------------|---------|
| `DownloadPool` | Network I/O | 4 | HTTP downloads with progress tracking |
| `UnpackPool` | Disk I/O | 2 | Archive extraction (.tar.gz, .tar.xz, .zip) |
| `InstallPool` | CPU | 2 | Verification, fingerprinting, post-install hooks |

### Data Flow

```
PackageTask(name, url, version, deps=[])
    │
    ▼
DependencyScheduler (resolves DAG, emits ready tasks)
    │
    ▼
DownloadPool ──progress──► PipelineProgressDisplay ("Downloading [=====>   ] 62%")
    │
    ▼
UnpackPool ──progress──► PipelineProgressDisplay ("Unpacking [========> ] 85%")
    │
    ▼
InstallPool ──status──► PipelineProgressDisplay ("Installing ⠸ Verifying...")
    │
    ▼
Done ──► PipelineProgressDisplay ("Done ✓ 3.2s")
```

### TUI Display

The Rich-based progress display shows a Docker pull-style multi-line live view:

```
Installing dependencies for env:uno...

  atmelavr 5.0.0           Downloading   [=========>          ]  45%  2.1 MB/s
  toolchain-atmelavr 3.1   Unpacking     [===============>    ]  78%
  framework-arduino 4.2.0  Installing    ⠸ Verifying toolchain binaries...
  Wire 1.0                 Done          ✓ 1.2s
  SPI 1.0                  Done          ✓ 0.8s
  Servo 1.1.8              Waiting

  6 packages, 3 active, 2 done
```

### Error Handling

- **Download retry**: Exponential backoff (3 attempts, 1s/2s/4s delays) for `ConnectionError`, `Timeout`, `OSError`
- **Extraction retry**: 3 attempts with 2s delay for `PermissionError` (Windows antivirus)
- **HTTP errors** (404, etc.): Not retried (permanent failures)
- **Ctrl-C cleanup**: Removes `.download` temp files and `temp_extract_*` directories
- **Dependency failure propagation**: Failed tasks cause dependent tasks to fail with descriptive messages

### Usage

```python
from fbuild.packages.pipeline import ParallelInstaller

installer = ParallelInstaller(
    download_workers=4,
    unpack_workers=2,
    install_workers=2,
)

result = installer.install_dependencies(
    project_path=Path("my_project"),
    env_name="uno",
    verbose=True,
    use_tui=None,  # Auto-detect TTY
)

print(f"Success: {result.success}, {result.completed_count} installed in {result.total_elapsed:.1f}s")
```

### Test Coverage

- `tests/unit/packages/pipeline/` - 264 unit tests covering models, scheduler, pools, pipeline, display, adapters, error handling
- Tests use mock downloads and run with `-n auto` for parallel execution

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

**Windows USB-CDC Serial Port Timeout Limitation**: On Windows, when esptool or other serial tools interact with ESP32 devices, the process can hang indefinitely if the device or USB-CDC driver is in a stuck state. This occurs because Windows serial port I/O operations can block in kernel space, making them immune to normal process termination signals.

**Root Cause**: When a process is blocked in a Windows kernel-level I/O operation (e.g., `ReadFile()` or `WriteFile()` on a serial port handle), the `subprocess.run(timeout=N)` mechanism cannot interrupt it. The termination signal (SIGTERM/SIGKILL) is not delivered until the I/O operation completes or the driver releases the handle.

**Symptoms**:
- Upload process hangs for 20+ minutes instead of timing out after 120 seconds
- Port remains locked even after killing the daemon
- No error message or timeout exception is raised
- Physical device reset required to unlock the port

**Solution** (implemented in v1.3.36+):
- **Watchdog Timeout**: The ESP32 deployer now uses `run_with_watchdog_timeout()` which monitors process output in real-time
- **Inactivity Detection**: If no output is received for 30 seconds, the process is forcefully terminated
- **Force Kill**: Uses `TerminateProcess()` on Windows for forceful termination when graceful termination fails
- **Better Error Messages**: Provides actionable guidance when timeout occurs

**Implementation**: See `src/fbuild/deploy/deployer_esp32.py:run_with_watchdog_timeout()`

**User Workarounds** (if issue persists):
1. Unplug and replug the USB cable
2. Try a different USB port (preferably USB 2.0, not USB 3.0)
3. Reset the device manually (hold BOOT button, press RESET)
4. Check Device Manager for driver issues (yellow exclamation marks)
5. Update USB-CDC drivers:
   - ESP32-S3 USB-Serial/JTAG: CH343/CH340 drivers
   - Other ESP32: CP210x or FTDI drivers
6. As a last resort, use PlatformIO directly with `--no-fbuild` flag

**Reference**: For detailed technical analysis, see `docs/windows_serial_limitations.md`

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
