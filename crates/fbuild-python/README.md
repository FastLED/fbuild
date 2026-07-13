# fbuild-python

PyO3 bindings exposing fbuild's Rust implementation as a Python module, API-compatible with the original Python fbuild package.

The crate keeps `pyo3`, `pyo3-build-config`, and `pyo3-async-runtimes` on the
0.29 release family and builds an `abi3-py310` extension for CPython 3.10+.
Cross builds suppress host-interpreter discovery with `PYO3_NO_PYTHON=1`; they
do not require a target Python installation or import library.

## Key Types

- `SerialMonitor` -- Python context manager for serial I/O via the daemon's WebSocket endpoint; supports `read_lines()`, `write()`, `run_until()`, `write_json_rpc()`, and line hooks
- `Daemon` -- Static methods for daemon lifecycle: `ensure_running()`, `stop()`, `status()`
- `DaemonConnection` -- Python context manager for build/deploy/monitor operations via the daemon's HTTP API
- `connect_daemon()` -- Factory function matching `from fbuild import connect_daemon`

## Architecture

Python classes are thin wrappers around HTTP/WebSocket calls to the fbuild daemon. `SerialMonitor` uses the process-shared `pyo3-async-runtimes` tokio runtime for async WebSocket operations, exposed as synchronous Python methods via `block_on()`. The module is registered as `fbuild._native` and re-exported through the Python package.

## Consumer Contract

FastLED imports `SerialMonitor` as a context manager:
```python
from fbuild.api import SerialMonitor
with SerialMonitor(port="COM13", baud_rate=115200) as mon:
    for line in mon.read_lines(timeout=30.0):
        print(line)
```
