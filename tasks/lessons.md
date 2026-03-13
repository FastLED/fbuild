# Lessons Learned

## 2026-03-13: Chocolatey PATH Conflict

**Problem**: Bare `cargo` on Windows resolves to Chocolatey's ancient 1.66 instead of rustup's 1.94.

**Fix**: `ci/trampoline.py` prepends `~/.cargo/bin` to PATH before invoking cargo. `tool_guard.py` hook blocks bare `cargo` commands. Always use `uv run cargo`.

## 2026-03-13: Clippy `-D warnings` Catches Everything

**Pattern**: Stub code with unused fields, imports, or match arms will fail clippy with `-D warnings`. Use `#[allow(dead_code)]` at crate level for stubs, or prefix unused fields with `_`.

**Rule**: Always run `uv run cargo clippy --workspace --all-targets -- -D warnings` before considering code done.

## 2026-03-13: PyO3 `__exit__` Signature

**Pattern**: PyO3 0.22+ requires explicit `#[pyo3(signature = (...))]` on `__exit__` methods with `Option<T>` parameters. Without it, clippy reports deprecated implicit defaults.

**Fix**: Add `#[pyo3(signature = (_exc_type=None, _exc_val=None, _exc_tb=None))]`.
