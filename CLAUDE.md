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
- Fixed CTRL-C deadlock in HTTP client - see `docs/fix_ctrl_c_deadlock.md`

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

# PlatformIO compatibility mode (bypasses daemon, runs pio directly)
fbuild build tests/uno -e uno --platformio          # build via pio
fbuild deploy tests/esp32c6 -e esp32c6 --platformio # upload via pio
fbuild monitor tests/esp32c6 -e esp32c6 --platformio # monitor via pio
fbuild tests/esp32c6 --platformio                    # default action via pio
```

## Architecture

CLI → HTTP → Daemon (FastAPI) → Processors → Platform Orchestrators → Compiler/Linker → firmware. See `docs/claude/architecture.md` for full diagram, protocols, and patterns.

**Key data flows:** Build (CLI→Daemon→Orchestrator→firmware), Deploy (CLI→Daemon→esptool/avrdude→Device), Packages (Orchestrator→Cache→Downloader→extracted).

## Critical Constraints

### Locking: Memory-Based Daemon Locks Only

**Do NOT use file-based locks** (`fcntl`, `msvcrt`, `.lock` files). All cross-process synchronization goes through the daemon's `ResourceLockManager`. Use `threading.Lock` for in-process synchronization only.

### NEVER Kill All Python Processes

The daemon is shared across projects and killing it blindly will interrupt builds, kill Claude Code's own process, and leave resources inconsistent.

**To stop the daemon gracefully:**

1. **HTTP API (preferred):**
   ```bash
   curl -X POST http://127.0.0.1:8765/api/daemon/shutdown
   ```

2. **Kill specific daemon PID:**
   ```bash
   curl -s http://127.0.0.1:8765/api/daemon/info | python -c "import sys,json; print(json.load(sys.stdin)['pid'])"
   taskkill //PID <pid> //F  # Windows
   kill <pid>                 # Linux/macOS
   ```

3. **Signal file (fallback):**
   ```bash
   touch ~/.fbuild/daemon/shutdown.signal
   ```

**NEVER run** `pkill python`, `taskkill /IM python.exe /F`, or any command that kills all Python processes.

### No Default Arguments Policy

**Default arguments are forbidden** in function/method signatures. All arguments must be explicit at call sites.

**Exceptions:** `None` as default for truly optional params; public API classes in `fbuild/__init__.py` for backwards compat.

```python
# BAD
def compile(source: Path, flags: List[str] = [], verbose: bool = False): ...

# GOOD
def compile(source: Path, flags: List[str], verbose: bool): ...

# GOOD - None for optional
def compile(source: Path, flags: List[str], output: Path | None = None): ...
```

See `docs/claude/coding-conventions.md` for full rationale and examples.

### Type-Safe Configuration

**Use @dataclass structures instead of dict.get()** for configuration objects. Use `frozen=True`, provide `from_dict()` class methods, validate required fields. See `docs/claude/coding-conventions.md` for patterns and nested dataclass examples.

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

### Subprocess Safety

**ALWAYS use safe subprocess wrappers** - never call `subprocess.run()`/`subprocess.Popen()` directly:

```python
from fbuild.subprocess_utils import safe_run, safe_popen, get_python_executable

result = safe_run(cmd, ...)           # instead of subprocess.run()
proc = safe_popen(cmd, ...)           # instead of subprocess.Popen()
cmd = [get_python_executable(), ...]  # instead of sys.executable (uses pythonw.exe on Windows)
```

Enforcement: The `SUB` lint plugin detects unsafe calls. See `docs/claude/coding-conventions.md` and `docs/subprocess_safety.md` for details.

### Thread-Safe Output

All output goes through `src/fbuild/output.py` using `contextvars` for thread safety. Build processor uses `contextvars.copy_context()` for isolation. See `docs/claude/coding-conventions.md` for implementation details.

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

## Adding New Parameters

When adding CLI parameters that flow through to orchestrators:

1. **Define in CLI**: Add to `BuildArgs` dataclass and argparse
2. **Add to Message**: Update `BuildRequest` in `messages.py`
3. **Update Client**: Pass parameter in `daemon/client.py`
4. **Extract in Processor**: Forward to orchestrator in `build_processor.py`
5. **Update Interface**: Add to `IBuildOrchestrator` in `orchestrator.py`
6. **Implement**: Add to all platform orchestrators
7. **Test**: Add integration tests in `test_parameter_flow.py`
8. **Validate**: Run `./lint` to verify signature compliance

See `docs/parameter_flow.md` for detailed examples and step-by-step instructions.

## Linting

Run `./lint` to execute all checks: ruff, black, isort, pyright, and 5 custom checks:
1. Orchestrator Signature Validation (`scripts/check_orchestrator_signatures.py`)
2. Message Serialization Checker (`scripts/check_message_serialization.py`)
3. KeyboardInterrupt Checker (`scripts/check_keyboard_interrupt.py`)
4. Sys.Path Checker (`scripts/check_sys_path.py`)
5. Subprocess Safety Checker (`scripts/check_subprocess_safety.py`)

See `docs/claude/coding-conventions.md` for linting architecture details.

## Reference Documentation

| Topic | File | Read when... |
|-------|------|-------------|
| System architecture & protocols | `docs/claude/architecture.md` | Cross-layer changes, new orchestrators |
| Daemon HTTP/WS API | `docs/claude/daemon-and-ipc.md` | Modifying daemon endpoints or client code |
| Coding conventions & patterns | `docs/claude/coding-conventions.md` | Writing new code or refactoring |
| Parallel pipeline & compilation | `docs/claude/parallel-systems.md` | Touching pipeline or --jobs |
| Known platform issues | `docs/claude/platform-known-issues.md` | Debugging platform-specific failures |
| Parameter flow (detailed) | `docs/parameter_flow.md` | Adding new CLI parameters |
| Subprocess safety (detailed) | `docs/subprocess_safety.md` | Creating subprocess calls |
| Linting plugins (detailed) | `docs/linting_plugins.md` | Modifying or adding lint rules |
| Daemon API (detailed) | `docs/daemon-api.md` | Daemon internals |
| Windows serial limitations | `docs/windows_serial_limitations.md` | ESP32 upload hangs on Windows |
