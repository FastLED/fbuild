# CLAUDE.md

fbuild is a PlatformIO-compatible embedded build tool (11 crates). See @docs/CLAUDE.md for which architecture doc to read based on what you're working on.

## Essential Rules

- **Always use a globally-installed `soldr` to execute Rust commands.** Bare cargo/rustc and legacy `uv run cargo` shims are blocked by hook. soldr uses `rustup which` to pick the rustup-managed toolchain from `rust-toolchain.toml`. The standard Cargo path is `soldr cargo ...`, so repo Rust builds get soldr's managed zccache path by default; do not add repo-specific `RUSTC_WRAPPER` wiring for normal builds. Install soldr globally via `uv tool install soldr` (or see https://github.com/zackees/soldr).
- **Always use `uv` for Python.** Bare `python`/`pip` are blocked by hook. Use `uv run ...` or `uv pip ...`.
- MSRV: 1.94.1 | Edition: 2021 | Toolchain: 1.94.1 pinned in `rust-toolchain.toml` (clippy + rustfmt)
- CI: Linux, macOS, Windows. All warnings denied (`RUSTFLAGS="-D warnings"`)
- Every directory with files must have a README.md (enforced by hook)

## Commands

```bash
uv run test                 # unit tests only
uv run test --full          # unit + stress + integration tests
uv run test -p <crate> -- <test_name>
soldr cargo check --workspace --all-targets
soldr cargo clippy --workspace --all-targets -- -D warnings
soldr cargo fmt --all
RUSTDOCFLAGS="-D warnings" soldr cargo doc --workspace --no-deps

# Public deploy API conventions
soldr cargo run -p fbuild-cli -- deploy tests/platform/uno -e uno --to emu
soldr cargo run -p fbuild-cli -- deploy tests/platform/uno -e uno --to emu --monitor
soldr cargo run -p fbuild-cli -- deploy tests/platform/esp32dev -e esp32dev-qemu --to emu --emulator qemu

# test-emu: build + run in emulator (CI-friendly, exits with emulator exit code)
soldr cargo run -p fbuild-cli -- test-emu tests/platform/uno -e uno
soldr cargo run -p fbuild-cli -- test-emu tests/platform/esp32s3 -e esp32s3 --timeout 10
soldr cargo run -p fbuild-cli -- test-emu tests/platform/mega -e megaatmega2560 --emulator simavr

# Per-file rustfmt
soldr rustfmt --check <file.rs>

# Board definition management
uv run python ci/validate_boards.py                    # validate against PlatformIO
uv run python ci/validate_boards.py --external         # compare against Arduino + Zephyr
uv run python ci/board_sources.py --search QUERY       # search all external sources
uv run python ci/board_sources.py --compare            # find boards missing from fbuild
uv run python ci/board_sources.py --list-arduino       # list Arduino package index boards
uv run python ci/board_sources.py --list-zephyr        # list Zephyr boards
soldr cargo run -p fbuild-config --bin enrich_boards  # enrich from local PlatformIO
```

## Distribution

Native binaries are built via GitHub Actions and downloaded locally for packaging. PyPI is the distribution channel — no Python in the runtime hot path.

```bash
# Build all platforms (triggers GH Actions, waits, downloads to dist/)
uv run python ci/build_dist.py --ref main

# Publish to PyPI
./publish
```

Optional wrapper-mode only; do not use for standard soldr builds:

```bash
uv run python ci/zccache_setup.py  # writes rustc-wrapper = "zccache"
```

## Hooks (enforced automatically)

All hooks are Python scripts in `ci/hooks/`, invoked via `uv run`:

- **UserPromptSubmit**: `ci/hooks/board_context.py` detects board-related prompts and injects skill guidance (board lookup workflow, external source URLs, relevant commands)
- **PreToolUse**: `ci/hooks/tool_guard.py` blocks bare Rust commands and any `uv run` invocation of `soldr`/`cargo` (must use a globally-installed `soldr` directly) and bare `python`/`pip` (must use `uv`) across supported shell tools, not just Bash
- **PostToolUse**: `ci/hooks/lint.py` auto-formats + runs clippy on edited .rs files
- **PostToolUse**: `ci/hooks/readme_guard.py` errors if directory lacks README.md
- **SessionStart**: `ci/hooks/check-on-start.py` captures git fingerprint
- **Stop**: `ci/hooks/code-review-on-stop.py` triggers `/code-review` skill if source files changed
- **Stop**: `ci/hooks/check-on-stop.py` runs full workspace lint + tests (skips if no changes)

## Skills

Custom Claude Code skills in `.claude/skills/`:

- **`/board-support`** — Diagnose and fix board definition issues. Searches fbuild's database, PlatformIO, Arduino package indices, and Zephyr boards. Auto-suggested by the `board_context.py` hook when board-related prompts are detected.
- **`/code-review`** — End-of-session code review. Checks for hardcoded values (should be in JSON), code that belongs in core instead of platform crates, board/MCU JSON quality, orchestrator completeness, and bugs. Auto-triggered by Stop hook.

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
- **Emulator CLI convention** — prefer `fbuild test-emu` for CI; `fbuild deploy --to emu [--emulator <kind>]` for interactive use; keep `--target` and `--qemu` only as compatibility aliases

## Reference Implementations

- **Python fbuild**: `~/dev/fbuild` (production) and `main` branch of this repo
- **zccache**: `~/dev/zccache` (Rust workspace pattern, CI, distribution)
- **FastLED**: `~/dev/fastled` (consumer of fbuild's serial API)
