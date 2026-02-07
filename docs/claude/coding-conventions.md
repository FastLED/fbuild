# Coding Conventions & Patterns

> Reference doc for Claude Code. Read when writing new code or refactoring existing code.

## No Default Arguments Policy

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

## Type-Safe Configuration with Dataclasses

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

## Thread-Safe Output System

**All output goes through `src/fbuild/output.py` which uses `contextvars` for thread safety.**

The output system uses Python's `contextvars` instead of module-level globals. This ensures concurrent builds don't interfere with each other's:
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

**DEPRECATED:** Module-level globals (`_start_time`, `_output_stream`, `_verbose`, `_output_file`) are kept for backward compatibility but will be removed in a future version. Always use the context API (`get_context()`, `set_output_file()`, etc.).

## Subprocess Safety

**ALWAYS use safe subprocess wrappers** to prevent console issues on Windows:

```python
# UNSAFE - Direct subprocess calls
result = subprocess.run(cmd, ...)
proc = subprocess.Popen(cmd, ...)

# SAFE - Use wrappers from subprocess_utils
from fbuild.subprocess_utils import safe_run, safe_popen

result = safe_run(cmd, ...)
proc = safe_popen(cmd, ...)
```

**CRITICAL: Use pythonw.exe for Python subprocess calls:**

```python
# UNSAFE - Uses python.exe (shows console window)
cmd = [sys.executable, "-m", "esptool", ...]

# SAFE - Uses pythonw.exe on Windows (no console window)
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

## Custom Linting Architecture

**Implementation:**
- Plugin implementations: `fbuild_lint/ruff_plugins/` (NOT distributed with package)
- Standalone runners: `scripts/check_*.py` (invoke plugins via AST parsing)
- All checks use AST analysis for zero runtime overhead
- Plugins are excluded from distributed packages to prevent global pollution

**Why Standalone Scripts?**
Previously, plugins were registered via flake8 entry points, which caused them to activate globally for all Python projects when fbuild was installed. Now, plugins are only invoked explicitly via standalone scripts during fbuild development, ensuring they don't affect other projects.

**Custom checks:**
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

**Run signature validation**:
```bash
python scripts/check_orchestrator_signatures.py
```

**Expected output**:
```
Validating orchestrator signatures...

[orchestrator_avr] BuildOrchestratorAVR
  Inherits from IBuildOrchestrator
  Implements build() method
  build() signature matches IBuildOrchestrator
  Has platform build method: _build_avr
  _build_avr signature matches PlatformBuildMethod protocol

[orchestrator_esp32] OrchestratorESP32
  Inherits from IBuildOrchestrator
  ...

All orchestrators validated successfully.
```
