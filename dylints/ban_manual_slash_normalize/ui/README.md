# `ban_manual_slash_normalize` — UI fixtures

`disallowed.rs` + `disallowed.stderr` prove the lint fires on the
canonical anti-pattern (`path.to_string_lossy().replace('\\', "/")`).
The lint test runner in [`../src/lib.rs`](../src/lib.rs) `#[test] fn ui`
compiles `disallowed.rs` with the lint enabled and diffs the diagnostic
against `disallowed.stderr`.

This directory is on the lint's own allowlist so the fixture can
contain the anti-pattern without the lint recursing on itself.
