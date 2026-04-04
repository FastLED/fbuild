# fbuild package

Top-level Python package that re-exports Rust PyO3 classes, providing a drop-in replacement for the original Python fbuild package.

## Modules

- **`__init__.py`** -- Re-exports `Daemon`, `DaemonConnection`, `connect_daemon`, and `__version__` from `_native`
- **`_native.pyd`** -- Compiled Rust extension built by the `fbuild-python` crate (not edited directly)
- **`api/`** -- Sub-package exposing `SerialMonitor` for serial port monitoring
