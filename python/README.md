# Python Shims

Thin Python wrapper modules that re-export classes from the Rust `_native` extension (`_native.pyd`/`.so`), built by the `fbuild-python` crate via PyO3. These provide API compatibility with the original Python fbuild package so that downstream consumers like FastLED can use `from fbuild import Daemon` or `from fbuild.api import SerialMonitor` without changes.

## Contents

- **`fbuild/`** -- Top-level package re-exporting `Daemon`, `DaemonConnection`, `connect_daemon`, and `__version__`
- **`fbuild/api/`** -- Public serial-monitoring API re-exporting `SerialMonitor`
- **`fbuild/_native.{pyd,abi3.so,so,dylib}`** -- Compiled Rust PyO3 extension (platform-specific binary, gitignored — see below)

## Building the native extension locally

The `_native` extension is **not** checked into the repo — it would go stale relative to `crates/fbuild-python/src/lib.rs`. Build it locally before running any Python test or script that imports `fbuild._native`:

```bash
# 1. Compile the PyO3 extension
soldr cargo build --release -p fbuild-python --features extension-module

# 2. Copy into python/fbuild/ (platform-specific extension name).
#    Rustup may place output under target/<triple>/release/ on some hosts —
#    adjust the source path if target/release/ does not contain the artifact.
#
#    Windows:
cp target/release/_native.dll python/fbuild/_native.pyd \
  || cp target/x86_64-pc-windows-msvc/release/_native.dll python/fbuild/_native.pyd
#    Linux:
cp target/release/lib_native.so python/fbuild/_native.abi3.so
#    macOS:
cp target/release/lib_native.dylib python/fbuild/_native.abi3.so
```

Rebuild whenever `crates/fbuild-python/src/lib.rs` (or any crate it depends on) changes. Published PyPI wheels are assembled by `ci/publish.py` using native binaries built in GitHub Actions, so this local step only affects in-tree Python tests.
