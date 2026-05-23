# ban_raw_subprocess

Custom dylint lint that bans raw `std::process::Command::{spawn, output, status}`
and `tokio::process::Command::{spawn, output, status}` in fbuild production
code. Every child process fbuild launches **must** go through:

| Use case                              | Wrapper                                            |
| ------------------------------------- | -------------------------------------------------- |
| Synchronous capture (compilers, etc.) | `fbuild_core::subprocess::run_command`             |
| Contained spawn (daemon-managed)      | `fbuild_core::containment::spawn_contained`        |
| Detached spawn (must outlive daemon)  | `fbuild_core::containment::spawn_detached`         |
| Tokio-spawned (emulator/qemu)         | `fbuild_core::containment::tokio_spawn::spawn_contained` |

Those wrappers go through `running-process-core` (via
`NativeProcess`) or apply the Windows Job Object containment we
require for daemon-launched children. Bypassing them silently regresses
one or more of: stdout/stderr drain (deadlock on full pipe buffers
— see #141), `CREATE_NO_WINDOW` (Windows console flash), Job Object
attach (orphaned children when daemon crashes), and originator-env
propagation (cross-process correlation).

## Opting out

Add the source-file path to `src/allowlist.txt` with a comment
explaining why the raw spawn is correct in that file. Allowlisting is
file-level — every spawn in the file is exempted, so prefer narrow,
single-purpose files.

## Running

```bash
cargo install cargo-dylint dylint-link --locked
cargo dylint --all -- --workspace --all-targets -- -D warnings
```

The workspace registers this dylint via `workspace.metadata.dylint`
in the root `Cargo.toml`.

## Why a separate nightly toolchain

`dylint_linting` links against `rustc_private` APIs that require a
specific nightly toolchain. The pin in `rust-toolchain.toml` here is
local to this dylint crate (it doesn't affect the fbuild workspace's
stable `1.94.1` pin) and matches zccache's `dylints/*` pin so we
share a single dylint loader across both repos.
