# `cli_no_build_deploy_direct_use`

Custom [dylint](https://github.com/trailofbits/dylint) that forbids
files under `crates/fbuild-cli/src/` from naming items in the
`fbuild_build::*` or `fbuild_deploy::*` paths, except in a small
allowlist of diagnostic subcommands.

## Why

The "thin HTTP client" rule (see `crates/CLAUDE.md` §"Dependency
Graph" and §"Diagnostic subcommand exception"): `fbuild-cli` is meant
to be a tiny CLI that sends JSON over HTTP to the daemon. The daemon
owns all build orchestration (`fbuild-build`) and all firmware upload
(`fbuild-deploy`). Importing those crates into the CLI silently
bypasses the daemon, defeats build streaming, breaks
deploy-preemption semantics, and produces inconsistent error
surfaces.

The exception is "diagnostic subcommands" — read-only in-process
inspectors like `clang-tidy`, `lib-select`, `symbols`, `bloat`,
`graph`, `lnk`, that don't need the build pipeline and would only add
latency by going through HTTP. Those subcommands are explicitly
allowlisted by file path.

## Scope

The lint runs only on `.rs` files under `crates/fbuild-cli/src/`. Any
file outside that scope is unconditionally exempt.

## Allowlist

Files allowed to import `fbuild_build::*` or `fbuild_deploy::*` are
listed in `src/allowlist.txt`. Each entry needs an inline comment
explaining what diagnostic the file implements.

Current entries reflect the state at the time #826 was implemented;
the target is to drive the list down as diagnostic subcommands grow
their own crate (or as runtime-only paths move into the daemon).

## Toolchain

Pinned to the same `nightly-2026-03-26` channel and the same
`trailofbits/dylint` git rev (`4bd91ce…`) the other fbuild dylints
use.

## Running locally

See `dylints/README.md` for the full local-run recipe. CI runs all
dylints on every push/PR via `.github/workflows/dylint.yml`.

## See also

- Issue #826 — this lint's tracking issue
- `crates/CLAUDE.md` — "Diagnostic subcommand exception"
