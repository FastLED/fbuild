# `ban_direct_serialport`

This lint bans direct references to items from the `serialport` crate in
fbuild production code (`crates/*/src/`) outside `crates/fbuild-serial/`
and a small set of diagnostic CLI entry points.

## Why

All serial-port access in fbuild must flow through `fbuild-serial`'s
blessed APIs so the Windows USB-CDC contract (30-retry open, aggressive
buffer drain, DTR/RTS toggling after flash — see CLAUDE.md "Windows
USB-CDC") stays in one place. Direct `serialport::` usage scattered
across crates regresses that invariant silently.

## Scope

The lint fires on path expressions whose resolved DefId is rooted at the
`serialport` crate (i.e. the first segment of the crate's def-path is
`serialport`). Only files whose path contains BOTH `crates/` and a
subsequent `/src/` segment are linted; tests/benches/examples are out of
scope by design.

`crates/fbuild-serial/` is unconditionally exempt — it IS the wrapper.

## Allowlist

Files in scope that legitimately need raw `serialport::` use are listed
in `src/allowlist.txt`. The target state is to migrate the rest into
`fbuild-serial`; new files should not be added.

## See also

- FastLED/fbuild#826 — the dylint sweep tracking issue
- `crates/fbuild-serial/` — the blessed wrapper
