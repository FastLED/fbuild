# fbuild package

Top-level Python package that re-exports Rust PyO3 classes, providing a drop-in replacement for the original Python fbuild package.

## Modules

- **`__init__.py`** -- Re-exports `Daemon`, `DaemonConnection`, `connect_daemon`, and `__version__` from `_native`
- **`_native.{pyd,abi3.so,so,dylib}`** -- Compiled Rust extension built by the `fbuild-python` crate (not checked into the repo — build locally; see [../README.md](../README.md))
- **`api/`** -- Sub-package exposing `SerialMonitor` for serial port monitoring
