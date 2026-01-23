# Custom Linting Plugins

This document describes the custom linting plugins created for fbuild to ensure code quality and consistency.

## Overview

fbuild includes four custom linting plugins:

1. **KeyboardInterrupt Checker (KBI)** - Ensures proper keyboard interrupt handling
2. **Orchestrator Signature Checker (OSC)** - Validates orchestrator build() method signatures
3. **Message Serialization Checker (MSC)** - Validates message serialization completeness
4. **Sys Path Insert Checker (SPI)** - Detects sys.path.insert() anti-pattern

## 1. KeyboardInterrupt Checker (KBI)

**Location:** `fbuild_lint/ruff_plugins/keyboard_interrupt_checker.py`

**Purpose:** Ensures that try-except blocks catching broad exceptions (Exception, BaseException) also properly handle KeyboardInterrupt to prevent CLI hangs.

**Error Codes:**
- `KBI001`: Try-except catches Exception/BaseException without KeyboardInterrupt handler
- `KBI002`: KeyboardInterrupt handler must call `_thread.interrupt_main()` or `handle_keyboard_interrupt_properly()`

**Usage:**
```bash
flake8 src --select=KBI
```

**Example:**
```python
# BAD - Will trigger KBI001
try:
    some_operation()
except Exception as e:
    handle_error(e)

# GOOD
try:
    some_operation()
except KeyboardInterrupt:
    raise
except Exception as e:
    handle_error(e)
```

## 2. Orchestrator Signature Checker (OSC)

**Location:** `fbuild_lint/ruff_plugins/orchestrator_signature_checker.py`

**Purpose:** Validates that all platform-specific orchestrators implement the correct build() method signature with all required parameters and type annotations.

**Error Codes:**
- `OSC001`: Missing required parameter in build() method
- `OSC002`: Incorrect parameter type annotation in build() method
- `OSC003`: Missing return type annotation in build() method

**Required build() signature:**
```python
def build(
    self,
    project_dir: Path,
    env_name: Optional[str] = None,
    clean: bool = False,
    verbose: Optional[bool] = None,
    jobs: int | None = None,
) -> BuildResult:
    ...
```

**Usage:**
```bash
python scripts/check_orchestrator_signatures.py
```

**Checked files:**
- `src/fbuild/build/orchestrator_avr.py`
- `src/fbuild/build/orchestrator_esp32.py`
- `src/fbuild/build/orchestrator_rp2040.py`
- `src/fbuild/build/orchestrator_stm32.py`
- `src/fbuild/build/orchestrator_teensy.py`

## 3. Message Serialization Checker (MSC)

**Location:** `fbuild_lint/ruff_plugins/message_serialization_checker.py`

**Purpose:** Validates that all dataclass message types properly serialize and deserialize all their fields in `to_dict()` and `from_dict()` methods.

**Error Codes:**
- `MSC001`: Dataclass field not included in from_dict() method
- `MSC002`: Dataclass field not serialized in to_dict() method

**Usage:**
```bash
python scripts/check_message_serialization.py
```

**Checked files:**
- `src/fbuild/daemon/messages.py`

**Smart detection:**
- Skips classes using `serialize_dataclass()` / `deserialize_dataclass()` helpers (they're automatically correct)
- Detects `asdict()` usage (serializes all fields)
- Validates explicit field-by-field serialization

**Example:**
```python
@dataclass
class MyMessage:
    field1: str
    field2: int

    def to_dict(self) -> dict[str, Any]:
        # MSC002 if field2 is missing
        return {"field1": self.field1}

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "MyMessage":
        # MSC001 if field2 is missing
        return cls(field1=data["field1"])
```

## 4. Sys Path Insert Checker (SPI)

**Location:** `fbuild_lint/ruff_plugins/sys_path_checker.py`

**Purpose:** Detects `sys.path.insert()` calls which typically indicate that code is trying to run Python directly without proper virtual environment activation. This anti-pattern can cause import issues and makes scripts dependent on specific directory structures.

**Error Codes:**
- `SPI001`: sys.path.insert() detected - use proper virtual environment activation instead

**Usage:**
```bash
flake8 src scripts tests --select=SPI --exclude='*/.fbuild/*,*/.build/*,*/.zap/*'
```

**Why it's an anti-pattern:**
- Scripts become dependent on being run from specific directories
- Breaks when the environment is not properly activated
- Makes it harder to understand which environment is being used
- Can cause subtle import issues and version conflicts

**Example:**
```python
# BAD - Will trigger SPI001
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).parent.parent))
from my_package import something

# GOOD - Use proper environment activation
# Run with: uv run python script.py
# Or: pip install -e . (development mode)
from my_package import something
```

**Proper solutions:**
1. Install the package in development mode: `pip install -e .`
2. Run scripts with `uv run python script.py`
3. Use `python -m` to run modules (ensures proper path setup)
4. Set PYTHONPATH environment variable if absolutely necessary

## Running All Checks

The `lint` script runs all linting checks including the custom plugins:

```bash
./lint
```

This executes:
1. ruff (src and tests)
2. black (src and tests)
3. isort (src and tests)
4. pyright (src and tests)
5. flake8 KBI checks
6. flake8 SPI checks
7. Orchestrator signature checks
8. Message serialization checks

## Implementation Notes

### Flake8 Integration

The plugins are registered as flake8 extensions via entry points in `pyproject.toml`:

```toml
[project.entry-points."flake8.extension"]
KBI = "fbuild_lint.ruff_plugins.keyboard_interrupt_checker:KeyboardInterruptChecker"
OSC = "fbuild_lint.ruff_plugins.orchestrator_signature_checker:OrchestratorSignatureChecker"
MSC = "fbuild_lint.ruff_plugins.message_serialization_checker:MessageSerializationChecker"
SPI = "fbuild_lint.ruff_plugins.sys_path_checker:SysPathChecker"
```

**Note:** Due to changes in flake8 7.x plugin loading, OSC and MSC checkers are currently run via standalone Python scripts rather than through flake8. The KBI and SPI checkers work correctly with flake8.

### Standalone Scripts

For OSC and MSC, standalone scripts are provided in `scripts/`:
- `scripts/check_orchestrator_signatures.py`
- `scripts/check_message_serialization.py`

These scripts:
1. Parse the relevant Python files into AST
2. Instantiate the checker
3. Run validation
4. Report errors with line numbers

## Adding New Checks

To add a new custom check:

1. Create a new plugin in `fbuild_lint/ruff_plugins/`
2. Follow the flake8 plugin interface:
   - `__init__(self, tree: ast.AST)`
   - `name` and `version` class attributes
   - `run()` method returning generator of (line, col, msg, type) tuples
3. Register in `pyproject.toml` entry points
4. Create standalone script in `scripts/` if flake8 integration doesn't work
5. Add to `lint` script

## Best Practices

1. **Error codes:** Use 3-digit codes (e.g., KBI001, OSC001)
2. **Messages:** Include actionable fix suggestions in error messages
3. **AST traversal:** Use `ast.NodeVisitor` for clean traversal logic
4. **Testing:** Test plugins with both valid and invalid code
5. **Documentation:** Document all error codes and their meanings
