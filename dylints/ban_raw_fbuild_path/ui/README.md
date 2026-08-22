# `ban_raw_fbuild_path` — UI fixtures

`disallowed.rs` + `disallowed.stderr` prove the lint fires on both
shapes of the anti-pattern: a `Path::join(".fbuild")` segment and a
`format!` template that spells the whole layout inline. The lint test
runner in [`../src/lib.rs`](../src/lib.rs) `#[test] fn ui` compiles
`disallowed.rs` with the lint enabled and diffs the diagnostics against
`disallowed.stderr`.

This directory is on the lint's own allowlist
([`../src/allowlist.txt`](../src/allowlist.txt)) so the fixture can
contain the anti-pattern without the lint recursing on itself.
