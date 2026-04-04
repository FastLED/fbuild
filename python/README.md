# Python Shims

Thin Python wrapper modules that re-export classes from the Rust `_native` extension (`_native.pyd`/`.so`), built by the `fbuild-python` crate via PyO3. These provide API compatibility with the original Python fbuild package so that downstream consumers like FastLED can use `from fbuild import Daemon` or `from fbuild.api import SerialMonitor` without changes.

## Contents

- **`fbuild/`** -- Top-level package re-exporting `Daemon`, `DaemonConnection`, `connect_daemon`, and `__version__`
- **`fbuild/api/`** -- Public serial-monitoring API re-exporting `SerialMonitor`
- **`fbuild/_native.pyd`** -- Compiled Rust PyO3 extension (platform-specific binary)
