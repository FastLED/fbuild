# Subprocess Safety

## Overview

The fbuild codebase uses safe subprocess wrappers to prevent ephemeral shell windows from flashing on Windows during compilation operations. This is enforced via a custom flake8 plugin.

## The Problem

On Windows, direct subprocess calls without proper creation flags cause console windows to briefly appear and disappear during operations like compilation, linking, and deployment. This creates a poor user experience.

## The Solution

**File**: `src/fbuild/subprocess_utils.py`

Two wrapper functions automatically apply platform-specific flags:

```python
from fbuild.subprocess_utils import safe_run, safe_popen

# Instead of:
result = subprocess.run(cmd, ...)
proc = subprocess.Popen(cmd, ...)

# Use:
result = safe_run(cmd, ...)
proc = safe_popen(cmd, ...)
```

### How It Works

The wrappers automatically add `subprocess.CREATE_NO_WINDOW` flag on Windows:

```python
def safe_run(cmd: list[str], **kwargs: Any) -> subprocess.CompletedProcess:
    """Execute subprocess.run with platform-specific flags."""
    default_flags = get_subprocess_creation_flags()  # CREATE_NO_WINDOW on Windows

    if "creationflags" in kwargs:
        kwargs["creationflags"] = kwargs["creationflags"] | default_flags
    elif default_flags:
        kwargs["creationflags"] = default_flags

    return subprocess.run(cmd, **kwargs)
```

### Benefits

1. **No Console Flashing**: Windows users don't see ephemeral console windows
2. **Cross-Platform**: No-op on Linux/macOS (returns 0 for creation flags)
3. **Composable**: Preserves custom creation flags via bitwise OR
4. **Drop-in Replacement**: Same signature as subprocess.run/Popen

## Enforcement: Flake8 Plugin

**File**: `fbuild_lint/ruff_plugins/subprocess_safety_checker.py`

A custom flake8 plugin (`SUB`) detects unsafe subprocess calls:

### Error Codes

- `SUB001`: Direct `subprocess.run()` - use `safe_run()`
- `SUB002`: Direct `subprocess.Popen()` - use `safe_popen()`
- `SUB003`: Direct `subprocess.call()` - use `safe_run()`
- `SUB004`: Direct `subprocess.check_call()` - use `safe_run()`
- `SUB005`: Direct `subprocess.check_output()` - use `safe_run()`

### Usage

```bash
# Check specific file
flake8 --select=SUB src/fbuild/build/compiler.py

# Check entire src directory
flake8 --select=SUB src/

# Integrated in lint script
./lint  # Includes SUB checks
```

### Excluded Files

The plugin automatically skips:
- `subprocess_utils.py` (implementation file)
- `test_subprocess_utils.py` (unit tests)

## Migration Guide

### Before (Unsafe)

```python
import subprocess

# Direct call - causes console flash on Windows
result = subprocess.run(
    ["gcc", "-c", "main.c"],
    capture_output=True,
    text=True
)

# Direct Popen - same issue
proc = subprocess.Popen(
    ["make", "clean"],
    stdout=subprocess.PIPE
)
```

### After (Safe)

```python
from fbuild.subprocess_utils import safe_run, safe_popen

# Safe wrapper - no console flash
result = safe_run(
    ["gcc", "-c", "main.c"],
    capture_output=True,
    text=True
)

# Safe Popen wrapper
proc = safe_popen(
    ["make", "clean"],
    stdout=subprocess.PIPE
)
```

## Integration Points

### Build System

- **Compiler**: `src/fbuild/build/compiler.py` - Uses `SubprocessManager` (which uses `safe_run`)
- **Linker**: `src/fbuild/build/linker.py` - Needs migration to `safe_run()`
- **Archive Creator**: `src/fbuild/build/archive_creator.py` - Uses `SubprocessManager`

### Daemon

- **SubprocessManager**: `src/fbuild/daemon/subprocess_manager.py` - Central subprocess execution (uses `safe_run`)
- **Client**: `src/fbuild/daemon/client.py` - Daemon startup (needs `safe_popen()`)
- **Daemon**: `src/fbuild/daemon/daemon.py` - Self-restart (needs `safe_popen()`)

### Deploy/Tools

- **Deployers**: ESP32, AVR deployers - Need migration
- **QEMU Runner**: `src/fbuild/deploy/qemu_runner.py` - Needs migration
- **Docker Utils**: `src/fbuild/deploy/docker_utils.py` - Needs migration

### Libraries

- **Library Compiler**: `src/fbuild/packages/library_compiler.py` - Needs migration
- **Library Manager**: `src/fbuild/packages/library_manager_esp32.py` - Needs migration

## Current Status

**Plugin Registered**: ✅ `pyproject.toml` entry point: `SUB`
**Lint Integration**: ✅ Added to `./lint` script
**Implementation**: ✅ `subprocess_utils.py` with `safe_run()` and `safe_popen()`

**Violations Found**: 22 unsafe subprocess calls in codebase (as of 2026-01-23)

### Files with Violations

1. `src/fbuild/build/linker.py` (1 violation)
2. `src/fbuild/daemon/client.py` (1 violation)
3. `src/fbuild/daemon/daemon.py` (1 violation)
4. `src/fbuild/deploy/deployer_esp32.py` (2 violations)
5. `src/fbuild/deploy/docker_utils.py` (13 violations)
6. `src/fbuild/deploy/qemu_runner.py` (2 violations)
7. `src/fbuild/ledger/board_ledger.py` (1 violation)
8. `src/fbuild/packages/library_compiler.py` (1 violation)
9. `src/fbuild/packages/library_manager_esp32.py` (1 violation)

## Best Practices

1. **Always Import**: Add `from fbuild.subprocess_utils import safe_run, safe_popen` at the top
2. **Direct Replacement**: Replace `subprocess.run()` → `safe_run()`, same for Popen
3. **No Behavior Change**: All parameters work identically
4. **Check Before Commit**: Run `./lint` or `flake8 --select=SUB src/` before committing

## Testing

Unit tests in `tests/unit/test_subprocess_utils.py` verify:
- Windows gets `CREATE_NO_WINDOW` flag
- Other platforms get no flags
- Custom creation flags are preserved via bitwise OR
- All subprocess methods are properly wrapped

## References

- Implementation: `src/fbuild/subprocess_utils.py`
- Plugin: `fbuild_lint/ruff_plugins/subprocess_safety_checker.py`
- Tests: `tests/unit/test_subprocess_utils.py`
- Registration: `pyproject.toml` (flake8.extension entry point)
