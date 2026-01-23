# Parameter Flow Architecture

This document explains how parameters flow through fbuild's architecture from CLI to orchestrator, with a detailed focus on the `jobs` parameter as an example of the complete parameter lifecycle.

## Table of Contents

1. [Overview](#overview)
2. [Architecture](#architecture)
3. [The jobs Parameter: A Complete Example](#the-jobs-parameter-a-complete-example)
4. [Context Manager Pattern](#context-manager-pattern)
5. [Adding New Parameters](#adding-new-parameters)
6. [Testing Parameter Flow](#testing-parameter-flow)
7. [Best Practices](#best-practices)

---

## Overview

**Parameter flow** refers to the process of passing configuration values from user input (CLI commands) through multiple system layers (daemon IPC, message serialization, request processors) to the final execution point (platform-specific orchestrators).

### Why This Matters

- **Type Safety**: Ensures parameters maintain correct types through serialization
- **Consistency**: All platforms handle parameters identically
- **Testability**: Clear parameter paths enable focused integration tests
- **Maintainability**: Well-defined flow simplifies debugging and extensions

### Key Principles

1. **Explicit is better than implicit**: Always declare parameter types
2. **Preserve semantics**: `None` vs `0` vs `1` may have different meanings
3. **Fail early**: Validate at CLI, not deep in the build system
4. **Document defaults**: Make default behavior obvious in code

---

## Architecture

Parameter flow follows this path through the system:

```
┌─────────────────────────────────────────────────────────────────┐
│ 1. CLI Layer (cli.py)                                           │
│                                                                  │
│    User Input: fbuild build --jobs 4                            │
│           ↓                                                      │
│    argparse: parsed_args.jobs = 4                               │
│           ↓                                                      │
│    BuildArgs(jobs=4)                                            │
└─────────────────────────────────────────────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────────┐
│ 2. Daemon Client (daemon/client.py)                             │
│                                                                  │
│    request_build(jobs=4)                                        │
│           ↓                                                      │
│    BuildRequest(jobs=4)  ← Dataclass with typed fields          │
│           ↓                                                      │
│    to_dict() → {"jobs": 4, ...}  ← Serialization                │
│           ↓                                                      │
│    Write to: build_request.json                                 │
└─────────────────────────────────────────────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────────┐
│ 3. Daemon (daemon/daemon.py)                                    │
│                                                                  │
│    Read: build_request.json                                     │
│           ↓                                                      │
│    from_dict(data) → BuildRequest(jobs=4)  ← Deserialization    │
│           ↓                                                      │
│    Route to: BuildRequestProcessor                              │
└─────────────────────────────────────────────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────────┐
│ 4. Build Processor (processors/build_processor.py)              │
│                                                                  │
│    execute_operation(request: BuildRequest)                     │
│           ↓                                                      │
│    orchestrator.build(jobs=request.jobs)                        │
└─────────────────────────────────────────────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────────┐
│ 5. Orchestrator (build/orchestrator_*.py)                       │
│                                                                  │
│    build(jobs: int | None)                                      │
│           ↓                                                      │
│    with managed_compilation_queue(jobs) as queue:               │
│        compiler.compile(..., compilation_queue=queue)           │
└─────────────────────────────────────────────────────────────────┘
```

### Data Format at Each Layer

| Layer | Format | Type Safety |
|-------|--------|-------------|
| CLI | `argparse.Namespace` | Weak (argparse types) |
| BuildArgs | `@dataclass` | Strong (Python types) |
| BuildRequest | `@dataclass` | Strong (Python types) |
| JSON (IPC) | `dict[str, Any]` | None (serialized) |
| BuildRequest (daemon) | `@dataclass` | Strong (Python types) |
| Orchestrator | Method parameter | Strong (Python types) |

---

## The jobs Parameter: A Complete Example

The `jobs` parameter controls parallel compilation and demonstrates all aspects of parameter flow.

### 1. CLI Definition

**File**: `src/fbuild/cli.py`

```python
@dataclass
class BuildArgs:
    """Arguments for the build command."""
    project_dir: Path
    environment: Optional[str] = None
    clean: bool = False
    verbose: bool = False
    jobs: Optional[int] = None  # ← Parameter definition

# Argparse configuration
build_parser.add_argument(
    "-j",
    "--jobs",
    type=int,
    default=None,
    help="Number of parallel compilation jobs (default: CPU count, use 1 for serial)",
)
```

**Semantics**:
- `jobs=None`: Use default parallelism (CPU count or daemon's shared queue)
- `jobs=1`: Force serial compilation (useful for debugging)
- `jobs=N`: Use N parallel workers (creates temporary queue)

### 2. Daemon Client Request Creation

**File**: `src/fbuild/daemon/client.py`

```python
def request_build(
    project_dir: Path,
    environment: str,
    clean_build: bool = False,
    verbose: bool = False,
    jobs: Optional[int] = None,  # ← Parameter received from CLI
) -> bool:
    """Send build request to daemon."""

    # Create request message
    request = BuildRequest(
        project_dir=str(project_dir),
        environment=environment,
        clean_build=clean_build,
        verbose=verbose,
        caller_pid=os.getpid(),
        caller_cwd=str(Path.cwd()),
        jobs=jobs,  # ← Passed to request
    )

    # Serialize and write to file
    BUILD_REQUEST_FILE.write_text(json.dumps(request.to_dict()))

    # Wait for daemon response...
```

### 3. BuildRequest Message Definition

**File**: `src/fbuild/daemon/messages.py`

```python
@dataclass
class BuildRequest:
    """Client → Daemon: Build request message.

    Attributes:
        project_dir: Absolute path to project directory
        environment: Build environment name
        clean_build: Whether to perform clean build
        verbose: Enable verbose build output
        caller_pid: Process ID of requesting client
        caller_cwd: Working directory of requesting client
        jobs: Number of parallel compilation jobs (None = CPU count)
        timestamp: Unix timestamp when request was created
        request_id: Unique identifier for this request
    """

    project_dir: str
    environment: str
    clean_build: bool
    verbose: bool
    caller_pid: int
    caller_cwd: str
    jobs: int | None = None  # ← Optional field with default
    timestamp: float = field(default_factory=time.time)
    request_id: str = field(default_factory=lambda: f"build_{int(time.time() * 1000)}")

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return serialize_dataclass(self)  # ← Automatic serialization

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "BuildRequest":
        """Create BuildRequest from dictionary."""
        return deserialize_dataclass(cls, data)  # ← Automatic deserialization
```

### 4. Serialization/Deserialization

**How it works**:

```python
# Serialization (BuildRequest → JSON)
request = BuildRequest(jobs=4, ...)
serialized = request.to_dict()
# Result: {"jobs": 4, "project_dir": "...", ...}

json_str = json.dumps(serialized)
# Result: '{"jobs": 4, "project_dir": "...", ...}'

# Deserialization (JSON → BuildRequest)
data = json.loads(json_str)
# Result: {"jobs": 4, "project_dir": "...", ...}

request = BuildRequest.from_dict(data)
# Result: BuildRequest(jobs=4, ...)
```

**Key Points**:
- `None` is preserved through serialization (becomes `null` in JSON)
- Type checking occurs during deserialization
- Missing optional fields use their defaults

### 5. Build Processor Extraction

**File**: `src/fbuild/daemon/processors/build_processor.py`

```python
def execute_operation(self, request: BuildRequest, context: DaemonContext) -> bool:
    """Execute the build operation."""

    # ... setup code ...

    # Create orchestrator
    orchestrator = orchestrator_class(cache=cache, verbose=request.verbose)

    # Call build with jobs parameter
    build_result = orchestrator.build(
        project_dir=Path(request.project_dir),
        env_name=request.environment,
        clean=request.clean_build,
        verbose=request.verbose,
        jobs=request.jobs,  # ← Extracted from request
    )

    return build_result.success
```

### 6. Orchestrator Interface

**File**: `src/fbuild/build/orchestrator.py`

```python
@runtime_checkable
class PlatformBuildMethod(Protocol):
    """Protocol defining the expected signature for internal _build_XXX() methods.

    The jobs parameter controls parallel compilation:
    - jobs=None: Use all CPU cores (default)
    - jobs=1: Force serial compilation
    - jobs=N: Use N parallel workers
    """

    def __call__(
        self,
        project_path: Path,
        env_name: str,
        target: str,
        verbose: bool,
        clean: bool,
        jobs: int | None = None,  # ← Required parameter
    ) -> BuildResult:
        """Execute platform-specific build."""
        ...

class IBuildOrchestrator(ABC):
    """Interface for build orchestrators."""

    @abstractmethod
    def build(
        self,
        project_dir: Path,
        env_name: Optional[str] = None,
        clean: bool = False,
        verbose: Optional[bool] = None,
        jobs: int | None = None,  # ← Required parameter
    ) -> BuildResult:
        """Execute complete build process."""
        pass
```

### 7. Platform-Specific Implementation

**File**: `src/fbuild/build/orchestrator_esp32.py`

```python
class OrchestratorESP32(IBuildOrchestrator):
    """ESP32 build orchestrator."""

    def build(
        self,
        project_dir: Path,
        env_name: Optional[str] = None,
        clean: bool = False,
        verbose: Optional[bool] = None,
        jobs: int | None = None,  # ← Received from build processor
    ) -> BuildResult:
        """Execute ESP32 build."""

        # Use context manager for automatic queue cleanup
        with managed_compilation_queue(jobs, verbose=verbose or False) as queue:
            # Queue is automatically managed:
            # - jobs=None: Uses daemon's shared queue
            # - jobs=1: Returns None (serial mode)
            # - jobs=N: Creates temporary queue with N workers

            return self._build_esp32(
                project_dir,
                env_name or "default",
                "firmware",
                verbose or False,
                clean,
                jobs,  # ← Passed to internal build method
            )

    def _build_esp32(
        self,
        project_path: Path,
        env_name: str,
        target: str,
        verbose: bool,
        clean: bool,
        jobs: int | None = None,
    ) -> BuildResult:
        """Internal ESP32 build implementation."""

        # ... compilation setup ...

        # Compiler automatically uses the queue from context manager
        compiler.compile_all_sources(
            sources=sources,
            compilation_queue=queue,  # From context manager
        )
```

---

## Context Manager Pattern

The `managed_compilation_queue()` context manager ensures proper resource cleanup for parallel compilation queues.

### Why Use a Context Manager?

**Problem**: Creating temporary compilation queues requires explicit cleanup to prevent resource leaks.

**Solution**: Wrap queue management in a context manager that handles:
1. Queue selection (serial, daemon, or temporary)
2. Automatic cleanup of temporary queues
3. Exception-safe shutdown

### Implementation

**File**: `src/fbuild/build/orchestrator.py`

```python
@contextlib.contextmanager
def managed_compilation_queue(jobs: int | None, verbose: bool = False):
    """Context manager for safely managing compilation queue lifecycle.

    Args:
        jobs: Number of parallel compilation jobs
              - None: Use CPU count (daemon queue or fallback)
              - 1: Serial mode (no queue)
              - N: Custom worker count (temporary queue)
        verbose: Whether to log queue selection and lifecycle events

    Yields:
        Optional[CompilationJobQueue]: The queue to use, or None for serial mode

    Example:
        with managed_compilation_queue(jobs=4, verbose=True) as queue:
            compiler.compile(..., compilation_queue=queue)
        # Queue automatically cleaned up here
    """
    queue, should_cleanup = get_compilation_queue_for_build(jobs, verbose)
    try:
        yield queue
    finally:
        if should_cleanup and queue:
            try:
                if verbose:
                    print(f"[Cleanup] Shutting down temporary queue with {queue.num_workers} workers")
                queue.shutdown_and_wait()
            except Exception as e:
                # Log error but don't mask original exception
                logging.error(f"Error during queue cleanup: {e}")
```

### Queue Selection Strategy

```python
def get_compilation_queue_for_build(
    jobs: int | None,
    verbose: bool = False
) -> tuple[Optional[CompilationJobQueue], bool]:
    """Get appropriate compilation queue based on jobs parameter.

    Returns:
        Tuple of (compilation_queue, should_cleanup)
    """

    # Case 1: Serial mode
    if jobs == 1:
        if verbose:
            print("[Sync Mode] Using serial compilation (jobs=1)")
        return None, False

    cpu_count = multiprocessing.cpu_count()

    # Case 2: Default parallelism (daemon's shared queue)
    if jobs is None or jobs == cpu_count:
        try:
            from fbuild.daemon.daemon import get_compilation_queue
            daemon_queue = get_compilation_queue()
            if daemon_queue:
                if verbose:
                    print(f"[Async Mode] Using daemon queue with {daemon_queue.num_workers} workers")
                return daemon_queue, False  # No cleanup needed
        except (ImportError, AttributeError):
            pass

        # Fallback to serial if daemon unavailable
        if verbose:
            print("[Sync Mode] Daemon queue not available, using synchronous compilation")
        return None, False

    # Case 3: Custom worker count (temporary queue)
    try:
        from fbuild.daemon.compilation_queue import CompilationJobQueue

        if verbose:
            print(f"[Async Mode] Creating temporary queue with {jobs} workers")

        temp_queue = CompilationJobQueue(num_workers=jobs)
        temp_queue.start()
        return temp_queue, True  # Cleanup required!
    except (ImportError, AttributeError) as e:
        logging.warning(f"Failed to create temporary queue: {e}")
        return None, False
```

### Usage Patterns

**Pattern 1: Orchestrator Entry Point**
```python
def build(self, project_dir: Path, ..., jobs: int | None = None) -> BuildResult:
    with managed_compilation_queue(jobs, verbose=self.verbose) as queue:
        # Queue is available throughout build
        return self._build_internal(...)
```

**Pattern 2: Testing (Mock Queue)**
```python
def test_with_custom_queue():
    with patch('fbuild.build.orchestrator.get_compilation_queue_for_build') as mock:
        mock_queue = Mock()
        mock.return_value = (mock_queue, True)

        with managed_compilation_queue(jobs=4) as queue:
            assert queue is mock_queue
            # Do work...

        # Verify cleanup was called
        mock_queue.shutdown_and_wait.assert_called_once()
```

---

## Adding New Parameters

Follow this checklist when adding new CLI parameters:

### Step 1: Define in CLI

**File**: `src/fbuild/cli.py`

```python
@dataclass
class BuildArgs:
    project_dir: Path
    # ... existing fields ...
    new_parameter: bool = False  # ← Add field with default

# Add argparse argument
build_parser.add_argument(
    "--new-parameter",
    action="store_true",
    help="Description of new parameter",
)
```

### Step 2: Add to Message Definition

**File**: `src/fbuild/daemon/messages.py`

```python
@dataclass
class BuildRequest:
    project_dir: str
    # ... existing fields ...
    new_parameter: bool = False  # ← Add with default

    # to_dict() and from_dict() automatically handle the new field
```

### Step 3: Update Daemon Client

**File**: `src/fbuild/daemon/client.py`

```python
def request_build(
    project_dir: Path,
    environment: str,
    # ... existing parameters ...
    new_parameter: bool = False,  # ← Add parameter
) -> bool:
    request = BuildRequest(
        project_dir=str(project_dir),
        environment=environment,
        # ... existing fields ...
        new_parameter=new_parameter,  # ← Pass to request
    )
```

### Step 4: Extract in Build Processor

**File**: `src/fbuild/daemon/processors/build_processor.py`

```python
def execute_operation(self, request: BuildRequest, context: DaemonContext) -> bool:
    # Extract from request
    new_param_value = request.new_parameter

    # Pass to orchestrator
    build_result = orchestrator.build(
        # ... existing parameters ...
        new_parameter=new_param_value,
    )
```

### Step 5: Update Orchestrator Interface

**File**: `src/fbuild/build/orchestrator.py`

```python
class IBuildOrchestrator(ABC):
    @abstractmethod
    def build(
        self,
        project_dir: Path,
        # ... existing parameters ...
        new_parameter: bool = False,  # ← Add to interface
    ) -> BuildResult:
        pass
```

### Step 6: Implement in Platform Orchestrators

**Files**: `orchestrator_avr.py`, `orchestrator_esp32.py`, etc.

```python
def build(
    self,
    project_dir: Path,
    # ... existing parameters ...
    new_parameter: bool = False,  # ← Add to signature
) -> BuildResult:
    # Use the parameter
    if new_parameter:
        # Special handling...

    return self._build_internal(...)
```

### Step 7: Add Tests

**File**: `tests/integration/test_parameter_flow.py`

```python
def test_new_parameter_reaches_orchestrator():
    """Verify new parameter flows from CLI to orchestrator."""
    build_request = BuildRequest(
        project_dir="/path",
        environment="test",
        new_parameter=True,  # ← Test with new parameter
        # ... other fields ...
    )

    # Test serialization
    serialized = build_request.to_dict()
    assert serialized["new_parameter"] is True

    # Test deserialization
    deserialized = BuildRequest.from_dict(serialized)
    assert deserialized.new_parameter is True
```

---

## Testing Parameter Flow

### Unit Tests

Test individual components in isolation:

```python
def test_build_request_serialization():
    """Test BuildRequest serialization preserves types."""
    request = BuildRequest(jobs=4, verbose=True, ...)

    # Serialize
    data = request.to_dict()
    assert data["jobs"] == 4
    assert data["verbose"] is True

    # Deserialize
    restored = BuildRequest.from_dict(data)
    assert restored.jobs == 4
    assert restored.verbose is True
```

### Integration Tests

Test parameter flow through multiple layers:

```python
def test_jobs_parameter_flow_end_to_end():
    """Test jobs parameter from CLI to orchestrator."""

    # 1. Simulate CLI input
    build_args = BuildArgs(
        project_dir=Path("/test"),
        jobs=4,
    )

    # 2. Create daemon request
    request = BuildRequest(
        project_dir=str(build_args.project_dir),
        jobs=build_args.jobs,
        ...
    )

    # 3. Serialize (IPC)
    serialized = json.dumps(request.to_dict())

    # 4. Deserialize (daemon side)
    loaded = BuildRequest.from_dict(json.loads(serialized))

    # 5. Verify parameter preserved
    assert loaded.jobs == 4
```

### System Tests

Test actual CLI commands:

```bash
# Run build with custom jobs parameter
fbuild build tests/esp32c6 -e esp32c6 --jobs 4

# Verify in build logs that parallel compilation was used
# Expected output: "[Async Mode] Creating temporary queue with 4 workers"
```

---

## Best Practices

### 1. Use Type Hints Everywhere

```python
# Good: Explicit types
def build(self, jobs: int | None = None) -> BuildResult:
    ...

# Bad: No type hints
def build(self, jobs=None):
    ...
```

### 2. Document Parameter Semantics

```python
@dataclass
class BuildRequest:
    """Build request message.

    Attributes:
        jobs: Number of parallel compilation jobs
              - None: Use default (CPU count)
              - 1: Force serial compilation
              - N: Use N parallel workers
    """
    jobs: int | None = None
```

### 3. Validate Early

```python
# In CLI layer
build_parser.add_argument(
    "--jobs",
    type=int,
    default=None,
    help="...",
)

# Validate immediately after parsing
if args.jobs is not None and args.jobs < 1:
    parser.error("--jobs must be >= 1")
```

### 4. Use Protocols for Interfaces

```python
@runtime_checkable
class PlatformBuildMethod(Protocol):
    """Protocol ensures all platform build methods accept jobs parameter."""

    def __call__(
        self,
        project_path: Path,
        env_name: str,
        target: str,
        verbose: bool,
        clean: bool,
        jobs: int | None = None,
    ) -> BuildResult:
        ...
```

### 5. Test Serialization Round-Trips

```python
def test_request_serialization():
    original = BuildRequest(jobs=4, ...)
    serialized = original.to_dict()
    restored = BuildRequest.from_dict(serialized)
    assert restored.jobs == original.jobs
```

### 6. Handle Defaults Explicitly

```python
# Good: Explicit default in dataclass
@dataclass
class BuildRequest:
    jobs: int | None = None  # None means "use default"

# Bad: Implicit default
@dataclass
class BuildRequest:
    jobs: int | None  # What does None mean here?
```

### 7. Use Context Managers for Resources

```python
# Good: Automatic cleanup
with managed_compilation_queue(jobs) as queue:
    compile_with_queue(queue)
# Queue automatically cleaned up

# Bad: Manual cleanup (error-prone)
queue = create_queue(jobs)
try:
    compile_with_queue(queue)
finally:
    if queue:
        queue.shutdown()  # May fail
```

### 8. Write Integration Tests

```python
@pytest.mark.integration
def test_parameter_flow():
    """Test parameter flows from CLI to orchestrator."""
    # Mock each layer and verify parameter passing
    ...
```

---

## Summary

Parameter flow in fbuild follows a clear, type-safe path:

1. **CLI**: User input → `BuildArgs` dataclass
2. **Client**: `BuildArgs` → `BuildRequest` message
3. **Serialization**: `BuildRequest.to_dict()` → JSON
4. **IPC**: JSON file written/read by daemon
5. **Deserialization**: JSON → `BuildRequest.from_dict()`
6. **Processor**: `BuildRequest` → orchestrator call
7. **Orchestrator**: Type-checked method call → build execution

Key principles:
- **Type safety**: Python type hints at every layer
- **Serialization**: Automatic via `SerializableMessage` protocol
- **Resource management**: Context managers for cleanup
- **Testing**: Unit, integration, and system tests
- **Consistency**: All parameters follow the same flow

When adding new parameters, follow the 7-step process and add corresponding tests to ensure end-to-end correctness.
