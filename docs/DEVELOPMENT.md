# Development

This document covers the developer workflow: testing, troubleshooting, and local setup. For architecture, see [architecture/overview.md](architecture/overview.md). For why the project exists, see [WHY.md](WHY.md).

## Testing

fbuild includes comprehensive integration tests. The Rust workspace uses `cargo test` via `soldr`, enforced by repo hooks.

```bash
# Run unit tests only
uv run test

# Run unit + stress + integration tests
uv run test --full

# Run a specific test in a specific crate
uv run test -p <crate> -- <test_name>
```

For the legacy Python test suite (used only for CI parity checks against the Python fbuild reference):

```bash
# Run all tests
pytest tests/

# Run integration tests only
pytest tests/integration/

# Run with verbose output
pytest -v tests/integration/
```

**Test Coverage**:
- Full build success path
- Incremental builds
- Clean builds
- Firmware size validation
- HEX format validation
- Error handling (missing config, syntax errors, etc.)

## Troubleshooting

### Build fails with "platformio.ini not found"

Make sure you're in the project directory or use `-d`:
```bash
fbuild build -d /path/to/project
```

### Build fails with checksum mismatch

Clear cache and rebuild:
```bash
rm -rf .fbuild/cache/
fbuild build
```

### Compiler errors in sketch

Check the error message for line numbers:
```
Error: src/main.ino:5:1: error: expected ';' before '}' token
```

Common issues:
- Missing semicolon
- Missing closing brace
- Undefined function (missing #include or prototype)

### Slow builds

- First build with downloads: 15-30s (expected)
- Cached builds: 2-5s (expected)
- Incremental: <1s (expected)

If slower, check:
- Network speed (for downloads)
- Disk speed (SSD recommended)
- Use `--verbose` to see what's slow

See [architecture/overview.md](architecture/overview.md) for additional architecture-level troubleshooting.

## Development setup

To develop fbuild, run `. ./activate.sh`

### Windows

This environment requires you to use `git-bash`.

### Toolchain

- MSRV: 1.75 | Edition: 2021
- Toolchain: 1.94.1 pinned in `rust-toolchain.toml` (clippy + rustfmt)
- CI: Linux, macOS, Windows. All warnings denied (`RUSTFLAGS="-D warnings"`)

### Linting

Use `soldr` directly through the repo-local uv environment (bare `cargo` / `rustc` and `uv run cargo` shims are blocked by hook):

```bash
uv run soldr cargo check --workspace --all-targets
uv run soldr cargo clippy --workspace --all-targets -- -D warnings
uv run soldr cargo fmt --all
```

The legacy Python linters (`./lint.sh` with `pylint`, `flake8`, `mypy`) remain for any Python utility code under `ci/`.

### Distribution

Native binaries are built via GitHub Actions and downloaded locally for packaging. PyPI is the distribution channel — no Python in the runtime hot path.

```bash
# Build all platforms (triggers GH Actions, waits, downloads to dist/)
uv run python ci/build_dist.py --ref main

# Publish to PyPI
./publish
```

### Python / PyO3 extension

The `fbuild` Python package wraps a Rust PyO3 extension built from `crates/fbuild-python`. The compiled binary (`python/fbuild/_native.{pyd,abi3.so,so,dylib}`) is **not** checked into the repo — it must be rebuilt whenever `crates/fbuild-python/src/lib.rs` changes, otherwise tests that import `fbuild._native` fail with `ModuleNotFoundError` or `AttributeError`.

```bash
# Build the extension and copy it into the Python package
uv run soldr cargo build --release -p fbuild-python --features extension-module

# Windows
cp target/release/_native.dll python/fbuild/_native.pyd
# Linux
cp target/release/lib_native.so python/fbuild/_native.abi3.so
# macOS
cp target/release/lib_native.dylib python/fbuild/_native.abi3.so
```

See [`../python/README.md`](../python/README.md) for more detail. PyPI wheels are assembled by `ci/publish.py` using native binaries built in GitHub Actions — this local step only affects in-tree Python tests and scripts.

### Hooks (enforced automatically)

See the root [CLAUDE.md](../CLAUDE.md) for the full list of PreToolUse / PostToolUse / Stop hooks under `ci/hooks/`.

## See also

- [../CLAUDE.md](../CLAUDE.md) — project rules and essential commands
- [../crates/CLAUDE.md](../crates/CLAUDE.md) — crate dependency graph and boundaries
- [architecture/overview.md](architecture/overview.md) — system architecture
- [DESIGN_DECISIONS.md](DESIGN_DECISIONS.md) — ADR-style decisions
