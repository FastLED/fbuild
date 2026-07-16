# Lessons Learned

## 2026-03-13: Chocolatey PATH Conflict

**Problem**: Bare `cargo` on Windows resolves to Chocolatey's ancient 1.66 instead of rustup's 1.94.

**Fix**: `soldr` resolves the rustup-managed toolchain directly, and `tool_guard.py` blocks bare `cargo` commands. Always use `uv run soldr cargo`.

## 2026-03-13: Clippy `-D warnings` Catches Everything

**Pattern**: Stub code with unused fields, imports, or match arms will fail clippy with `-D warnings`. Use `#[allow(dead_code)]` at crate level for stubs, or prefix unused fields with `_`.

**Rule**: Always run `uv run soldr cargo clippy --workspace --all-targets -- -D warnings` before considering code done.

## 2026-03-13: PyO3 `__exit__` Signature

**Pattern**: PyO3 0.22+ requires explicit `#[pyo3(signature = (...))]` on `__exit__` methods with `Option<T>` parameters. Without it, clippy reports deprecated implicit defaults.

**Fix**: Add `#[pyo3(signature = (_exc_type=None, _exc_val=None, _exc_tb=None))]`.

## 2026-04-25: Match Upstream Semantics, Don't Re-derive Them (#205)

**Problem**: The original #205 plan called for "fixed-point over include closure (typically 2–3 iterations)" for library selection. PlatformIO LDF is not a fixed-point — it's BFS + ONE reconciliation pass (piolib.py:1156), with unconverged deps dropping silently.

**Lesson**: When replicating an upstream tool's behavior (PlatformIO, Arduino-CLI, etc.), read the source first and match its semantics exactly. Users who flip between fbuild and the upstream tool should see byte-identical output, not a "more correct" reinterpretation.

**Bonus**: Path-prefix attribution beats basename matching for library resolution. A library is selected only if the walker resolves an include *into* its `include_dirs`, not because some header shares a basename. Closes #202 and #204.

## 2026-07-15: Explain in Human Terms First (#1076)

**Problem**: Research summaries and issue comments written as compressed engineering notes ("byte-equivalent TU", "query-driver glob degenerates to ./*", file:line citations mid-sentence) made the user ask twice: "this is hard to understand" / "tell me in human terms".

**Rule**: Lead with what it means for a person using the tool, in plain sentences. Put the mechanism in a worked example (real sketch → what gets generated → what the editor sees) instead of abstract nouns. Keep file:line references out of prose meant for humans — collect them in a details/sources section.
