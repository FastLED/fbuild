# CLAUDE.md

fbuild is a PlatformIO-compatible embedded build tool (11 crates). See @docs/CLAUDE.md for which architecture doc to read based on what you're working on.

## Essential Rules

- **Always use `uv run` or `_cargo`/`_rustc`/`_rustfmt` trampolines to execute Rust commands.** Bare cargo/rustc are blocked by hook. Both `uv run` trampolines (via `pyproject.toml`) and shell trampolines (`_cargo`, `_rustc`, `_rustfmt`) prepend `~/.cargo/bin` to PATH, ensuring the rustup-managed toolchain is always used.
- **Always use `uv` for Python.** Bare `python`/`pip` are blocked by hook. Use `uv run ...` or `uv pip ...`.
- MSRV: 1.75 | Edition: 2021 | Toolchain: 1.94.1 pinned in `rust-toolchain.toml` (clippy + rustfmt)
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

# Shell trampolines (alternative to uv run for Rust tools)
./_cargo check --workspace --all-targets
./_cargo clippy --workspace --all-targets -- -D warnings
./_rustfmt --check <file.rs>

# Local zccache setup (optional, configures rustc-wrapper)
uv run python ci/zccache_setup.py

# Board definition management
uv run python ci/validate_boards.py                    # validate against PlatformIO
uv run python ci/validate_boards.py --external         # compare against Arduino + Zephyr
uv run python ci/board_sources.py --search QUERY       # search all external sources
uv run python ci/board_sources.py --compare            # find boards missing from fbuild
uv run python ci/board_sources.py --list-arduino       # list Arduino package index boards
uv run python ci/board_sources.py --list-zephyr        # list Zephyr boards
uv run cargo run -p fbuild-config --bin enrich_boards  # enrich from local PlatformIO
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

- **UserPromptSubmit**: `ci/hooks/board_context.py` detects board-related prompts and injects skill guidance (board lookup workflow, external source URLs, relevant commands)
- **PreToolUse**: `ci/hooks/tool_guard.py` blocks bare Rust commands (must use `uv run` or `_cargo`/`_rustc`/`_rustfmt` trampolines) and bare `python`/`pip` (must use `uv`)
- **PostToolUse**: `ci/hooks/lint.py` auto-formats + runs clippy on edited .rs files
- **PostToolUse**: `ci/hooks/readme_guard.py` errors if directory lacks README.md
- **SessionStart**: `ci/hooks/check-on-start.py` captures git fingerprint
- **Stop**: `ci/hooks/code-review-on-stop.py` triggers `/code-review` skill if source files changed
- **Stop**: `ci/hooks/check-on-stop.py` runs full workspace lint + tests (skips if no changes)

## Skills

Custom Claude Code skills in `.claude/skills/`:

- **`/board-support`** — Diagnose and fix board definition issues. Searches fbuild's database, PlatformIO, Arduino package indices, and Zephyr boards. Auto-suggested by the `board_context.py` hook when board-related prompts are detected.
- **`/code-review`** — End-of-session code review. Checks for hardcoded values (should be in JSON), code that belongs in core instead of platform crates, and bugs. Auto-triggered by Stop hook.

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
