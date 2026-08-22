# `ban_raw_fbuild_path` — UI fixtures

`disallowed.rs` + `disallowed.stderr` prove the lint fires on both
shapes of the anti-pattern: a `Path::join(".fbuild")` segment and a
`format!` template that spells the whole layout inline. The lint test
runner in [`../src/lib.rs`](../src/lib.rs) `#[test] fn ui` compiles
`disallowed.rs` with the lint enabled and diffs the diagnostics against
`disallowed.stderr`.

The fixture is deliberately *not* on
[`../src/allowlist.txt`](../src/allowlist.txt) — it has to trip the lint
for the test to mean anything. Nothing under `dylints/` is scanned by
the workspace sweep anyway: every lint crate is in the root
`Cargo.toml`'s `exclude` list.
