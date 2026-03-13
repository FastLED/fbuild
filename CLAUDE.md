# CLAUDE.md

fbuild is a PlatformIO-compatible embedded build tool (11 crates). See @docs/CLAUDE.md for which architecture doc to read based on what you're working on.

## Essential Rules

- **Always use `uv run` to execute Rust commands.** Bare cargo/rustc are blocked by hook. Trampolines in `pyproject.toml` ensure the correct toolchain.
- **Always use `uv` for Python.** Bare `python`/`pip` are blocked by hook. Use `uv run ...` or `uv pip ...`.
- MSRV: 1.75 | Edition: 2021 | Toolchain: stable (clippy + rustfmt)
- CI: Linux, macOS, Windows. All warnings denied (`RUSTFLAGS="-D warnings"`)
- Every directory with files must have a README.md (enforced by hook)

## Commands

```bash
uv run test                 # unit tests only
uv run test --full          # unit + stress + integration tests
uv run test -p <crate> -- <test_name>
uv run cargo check --workspace --all-targets
uv run cargo clippy --workspace --all-targets -- -D warnings
uv run cargo fmt --all
RUSTDOCFLAGS="-D warnings" uv run cargo doc --workspace --no-deps
```

## Distribution

Native binaries are built via GitHub Actions and downloaded locally for packaging. PyPI is the distribution channel — no Python in the runtime hot path.

```bash
# Build all platforms (triggers GH Actions, waits, downloads to dist/)
uv run python ci/build_dist.py --ref main

# Publish to PyPI
./publish
```

## Hooks (enforced automatically)

All hooks are Python scripts in `ci/hooks/`, invoked via `uv run`:

- **PreToolUse**: `ci/hooks/tool_guard.py` blocks bare Rust commands (must use `uv run`) and bare `python`/`pip` (must use `uv`)
- **PostToolUse**: `ci/hooks/lint.py` auto-formats + runs clippy on edited .rs files
- **PostToolUse**: `ci/hooks/readme_guard.py` errors if directory lacks README.md
- **SessionStart**: `ci/hooks/check-on-start.py` captures git fingerprint
- **Stop**: `ci/hooks/check-on-stop.py` runs full workspace lint + tests (skips if no changes)

## Language Policy

- **Python is only for CI scripts, packaging, hooks, and PyO3 bindings.** All tests, benchmarks, and application logic must be written in Rust.
- `uv run` is required only because hooks enforce it for toolchain management — it is not an endorsement of Python for project code.
- Exception: `fbuild-python` crate provides PyO3 bindings so FastLED can `from fbuild.api import SerialMonitor`.
- When in doubt, write it in Rust.

## Development Philosophy: TDD

- **Red → Green → Refactor.** Write failing tests first, then implement the minimum code to make them pass, then refactor.
- Tests are the spec. If the test suite passes, the feature works. If behavior isn't tested, it doesn't exist.
- Comprehensive tests over comprehensive docs. Tests are executable documentation.
- Test real behavior: use `tempfile` for filesystem tests, not mocks. Test the contract, not the implementation.
- **A/B testing**: FastLED can switch between `--platformio` and fbuild. The Python integration tests in `~/dev/fbuild/tests/` are the acceptance criteria.

## Core Principles

- Simplicity first. Minimal code impact. No over-engineering.
- No laziness. Root causes only. Senior developer standards.
- Verify before done. Run tests, demonstrate correctness.
- Plan non-trivial work in `tasks/todo.md`. Capture lessons in `tasks/lessons.md`.

## Key Constraints

- **No file-based locks** — all locking through daemon's in-memory managers
- **Dev mode isolation** — `FBUILD_DEV_MODE=1` → port 8865, `~/.fbuild/dev/`
- **HTTP API compatibility** — same endpoints and JSON schemas as the Python daemon
- **Windows USB-CDC** — 30 retries, aggressive buffer drain, DTR/RTS toggling after flash

## Reference Implementations

- **Python fbuild**: `~/dev/fbuild` (production) and `main` branch of this repo
- **zccache**: `~/dev/zccache` (Rust workspace pattern, CI, distribution)
- **FastLED**: `~/dev/fastled` (consumer of fbuild's serial API)
