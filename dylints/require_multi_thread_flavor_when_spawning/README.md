# `require_multi_thread_flavor_when_spawning`

Custom [dylint](https://github.com/trailofbits/dylint) that flags
`#[tokio::test]` (or `#[tokio::test(...)]` *without* `flavor =
"multi_thread"`) on an `async fn` whose body calls `tokio::spawn(...)`
anywhere.

## Why

The default `tokio::test` flavor is `current_thread`, which runs all
spawned tasks on the same executor thread as the test itself. If a
spawned task and the test body both need to be polled to make
progress (e.g. the test awaits a channel the spawned task writes
to), the test deadlocks under load and runs serially otherwise — the
exact wrong shape for "I'm testing concurrent code".

The fix is one line:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn my_concurrent_test() { ... }
```

This lint catches the bug class at lint time instead of waiting for a
flaky CI run.

## Scope

The lint runs on all Rust files in the workspace. There is no
file-path scoping — the contract is "any tokio test that spawns must
use multi_thread", regardless of where the test lives.

## Allowlist

Spans flagged by this lint are listed in `src/allowlist.txt` by source
file path (the lint matches on the tail of the file path). The current
entries are the legacy violators that existed when the lint was first
enabled (#826) — they need to migrate to `flavor = "multi_thread"` and
their allowlist entries should be removed.

## Known limitations

- The lint only inspects the function body lexically — it does not
  follow calls into helper functions that themselves call
  `tokio::spawn`. If you wrap `tokio::spawn` in a helper, the test
  itself will not be flagged.
- The lint trusts the attribute's literal `flavor` string — it does
  not resolve constants or macros that could produce the attribute.
  In practice attributes are written as literals.

## Toolchain

Pinned to the same `nightly-2026-03-26` channel and the same
`trailofbits/dylint` git rev (`4bd91ce…`) the other fbuild dylints
use.

## Running locally

See `dylints/README.md` for the full local-run recipe. CI runs all
dylints on every push/PR via `.github/workflows/dylint.yml`.

## See also

- Issue #826 — this lint's tracking issue
