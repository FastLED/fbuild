# `ban_raw_subprocess` — sources

- **`lib.rs`** — late-pass `LintContext` visitor that fires on:
  - `ExprKind::MethodCall` whose resolved `type_dependent_def_id` matches
    one of `BANNED_METHOD_PATHS` (catches `cmd.spawn()` / `cmd.output()`
    / `cmd.status()`)
  - `ExprKind::Path` whose `qpath_res` resolves to a banned associated
    function (catches `Command::spawn(&mut cmd)`, `<Command>::output(...)`,
    `tokio::process::Command::status(...)`, etc.)

  Matching is exact — any other method on `Command` (`args`, `env`,
  `current_dir`) or on `Child` (`wait_with_output`, `kill`, `try_wait`)
  is intentionally not banned.

  Scope is restricted to files whose path contains BOTH `crates/` and a
  subsequent `/src/` segment, via `in_production_scope`. This excludes
  `crates/*/tests/`, `crates/*/examples/`, `crates/*/benches/`, `ci/`,
  `dylints/`, and anything outside `crates/`. (See #264 CR blocker #2:
  the prior prototype's `SOURCE_PREFIX = "crates/"` was too loose and
  flagged test files that legitimately spawn binaries under test.)

- **`allowlist.txt`** — newline-separated tail-suffix matches for source
  files that legitimately need to call the banned APIs (the blessed
  helpers themselves; daemon-bootstrap spawns from the CLI/Python;
  cross-tool zccache daemon launch; CLI-side async fan-out where no
  containment group exists). New entries require a justification
  comment.

The `_in_daemon` suffix that zccache's sibling crate uses isn't applied
here because fbuild's wrappers are used by *every* crate, not just the
daemon — the scope is "production code under `crates/*/src/`".
