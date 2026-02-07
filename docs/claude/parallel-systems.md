# Parallel Systems

> Reference doc for Claude Code. Read when touching the package pipeline or --jobs compilation.

## Parallel Package Pipeline

The parallel package pipeline (`src/fbuild/packages/pipeline/`) provides concurrent package installation with a Docker pull-style TUI display.

### Directory Structure

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
InstallPool ──status──► PipelineProgressDisplay ("Installing ... Verifying...")
    │
    ▼
Done ──► PipelineProgressDisplay ("Done 3.2s")
```

### TUI Display

The Rich-based progress display shows a Docker pull-style multi-line live view:

```
Installing dependencies for env:uno...

  atmelavr 5.0.0           Downloading   [=========>          ]  45%  2.1 MB/s
  toolchain-atmelavr 3.1   Unpacking     [===============>    ]  78%
  framework-arduino 4.2.0  Installing    Verifying toolchain binaries...
  Wire 1.0                 Done          1.2s
  SPI 1.0                  Done          0.8s
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

**Validated Platforms** (parallel compilation tested and working):
- **AVR** (Arduino Uno, etc.) - Integration tests: `tests/integration/test_parallel_uno.py`
- **Teensy** (Teensy 4.1, etc.) - Integration tests: `tests/integration/test_parallel_teensy.py`
- **ESP32** (ESP32dev, ESP32C6, etc.) - Integration tests: `tests/integration/test_parallel_esp32.py`

### Performance

Parallel compilation provides significant speedups on multi-core systems:
- **Teensy 4.1**: 11.8x faster (991.9s serial → 83.5s with --jobs 2)
- **AVR Uno**: ~2-3x faster on typical projects
- **ESP32**: Modest improvements (4-10% faster due to smaller core size)

Actual speedup depends on:
- Number of CPU cores
- Project size (more source files = better parallelization)
- I/O performance (Windows file locking can reduce gains)
