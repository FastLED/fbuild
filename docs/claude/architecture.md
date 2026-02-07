# Architecture & Protocols

> Reference doc for Claude Code. Read when making cross-layer changes or adding new orchestrators.

## System Architecture

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

## SerializableMessage Protocol

All daemon messages implement the `SerializableMessage` protocol for type-safe serialization.

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

## PlatformBuildMethod Protocol

All platform-specific build methods follow the `PlatformBuildMethod` protocol signature.

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

## managed_compilation_queue() Context Manager

Handles compilation queue lifecycle and resource cleanup.

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

**Serial Mode**: Only occurs when user explicitly requests `--jobs 1` for debugging. This is NOT a fallback - it's an intentional design choice.

## Daemon Spawn Race Condition Handling

**Status**: FIXED in v1.3.31 (2026-01-29)

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
