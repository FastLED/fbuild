# `ban_process_exit_outside_main`

Custom [dylint](https://github.com/trailofbits/dylint) that forbids
calling `std::process::exit(_)` (and its `process::exit` re-imports)
outside the canonical entry-point files: `crates/*/src/main.rs` and
`crates/*/src/bin/*.rs`.

## Why

`std::process::exit` skips destructors. In fbuild that translates to:

- Temp files in the cache root that were created via `NamedTempFile` /
  `TempDir` are not unlinked → cache grows without bound.
- `running-process` containment guards do not run their drop handlers →
  child processes (esptool / qemu / simavr) may keep running after the
  parent exits.
- Tokio runtime is dropped abruptly, so in-flight HTTP/WS responses are
  truncated and clients see EOF mid-response.

The only places where `process::exit` is safe are the program entry
points (`main.rs` / `src/bin/*.rs`) where the runtime is being torn
down anyway. Everywhere else, return a `Result` (or `FbuildError`) and
let the entry point translate it into an exit code in one place.

## Scope

Only files whose path contains BOTH `crates/` and a subsequent `/src/`
segment are linted. Out of scope by design:

- `crates/*/tests/`, `crates/*/examples/`, `crates/*/benches/`
- `ci/`, `dylints/`, build scripts
- Anything outside `crates/`

Within scope, **two exemptions are unconditional** (no allowlist entry
needed):

- `crates/*/src/main.rs` — every crate's library/binary entry point.
- `crates/*/src/bin/*.rs` — any extra binary in `src/bin/`.

## Allowlist

Files in scope that legitimately need `process::exit` (for example,
subcommand dispatchers that have already printed an error and want to
short-circuit `main`'s return path) are listed in `src/allowlist.txt`.
Each entry needs an inline comment explaining why.

## Toolchain

Pinned to the same `nightly-2026-03-26` channel and the same
`trailofbits/dylint` git rev (`4bd91ce…`) the other fbuild dylints use.

## Running locally

See `dylints/README.md` for the full local-run recipe. CI runs all
dylints on every push/PR via `.github/workflows/dylint.yml`.

## See also

- Issue #826 — this lint's tracking issue (gotcha sweep)
