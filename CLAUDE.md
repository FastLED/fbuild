# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

fbuild is a PlatformIO-compatible embedded development tool providing build, deploy, and monitor functionality for Arduino/ESP32 platforms. It uses URL-based package management and a daemon for cross-process coordination.

**Current Version:** v1.3.10 (update in `src/fbuild/__init__.py`, `pyproject.toml`, and this file)

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
1. **Build**: CLI → Daemon → Build Processor → Orchestrator → Compiler → Linker → firmware
2. **Deploy**: CLI → Daemon → Deploy Processor → Deployer (esptool/avrdude) → Device
3. **Packages**: Orchestrator → Cache → Downloader → fingerprint → extracted packages

## Critical Constraints

### Locking Strategy: Memory-Based Daemon Locks Only

**Do NOT use file-based locks** (`fcntl`, `msvcrt`, `.lock` files). All cross-process synchronization goes through the daemon's `ResourceLockManager`. Use `threading.Lock` for in-process synchronization only.

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

**Serial Mode**: Only occurs when user explicitly requests `--jobs 1` for debugging.
This is NOT a fallback - it's an intentional design choice.

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
./lint  # Runs: ruff, black, isort, pyright, flake8, custom checks
```

**Custom checks**:
1. **Orchestrator Signature Validation** (`tools/validate_orchestrator_signatures.py`)
   - Ensures all platform orchestrators implement `IBuildOrchestrator` interface
   - Validates internal build methods follow `PlatformBuildMethod` protocol
   - Checks for required parameters (including `jobs`)

2. **Subprocess Safety Checker** (`fbuild_lint/ruff_plugins/subprocess_safety_checker.py`)
   - Detects direct `subprocess.run()` / `subprocess.Popen()` calls
   - Enforces use of `safe_run()` / `safe_popen()` from `subprocess_utils.py`
   - Prevents ephemeral console windows on Windows
   - Error codes: SUB001-SUB005
   - See: `docs/subprocess_safety.md`

3. **Message Protocol Validation** (planned)
   - Verifies all daemon messages implement `SerializableMessage` protocol
   - Checks for proper enum handling in serialization

**Run signature validation**:
```bash
python tools/validate_orchestrator_signatures.py
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

**What the wrappers do:**
1. **Prevent console window flashing**: Applies `CREATE_NO_WINDOW` flag on Windows
2. **Prevent keystroke loss**: Auto-redirects stdin to `subprocess.DEVNULL` to prevent child processes from stealing keyboard input

**stdin Auto-Redirect:**
- By default, stdin is redirected to `subprocess.DEVNULL`
- Prevents child processes from inheriting the parent's console input handle
- Fixes issues where background processes steal keystrokes from the terminal
- Can be overridden with explicit `stdin=` parameter if needed (e.g., for interactive processes)

**Enforcement**: The `SUB` flake8 plugin (run via `./lint`) detects unsafe subprocess calls.

**Details**: See `docs/subprocess_safety.md` for complete documentation and `INVESTIGATION.md` for the technical analysis of the keystroke loss issue.
