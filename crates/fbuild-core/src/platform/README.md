# Host platform boundary

This directory is the single host-platform boundary for fbuild. `mod.rs`
contains the only production host selector. The public capability modules
expose neutral APIs; the private `windows`, `linux`, and `macos` trees own
native implementation details.

Embedded board/MCU selection and host artifact policy do not belong here.

`host` exposes the current `HostPlatform` plus explicit values used by pure
product-owner tests. `executable` owns native executable and command-script
spelling. Product crates keep URL/checksum tables and embedded-target choices;
they pass or read neutral host facts instead of using raw `cfg!` queries.

`fs` owns host path and file identity, display rules, executable permissions,
link/reparse classification, volume facts, native error classification, shared
destination opening, atomic replacement, and blocked-I/O retirement. Cache
sizing, archive traversal, authorization, locking, diagnostics, and retry policy
remain with their product owners.

`ipc` owns fbuild local-endpoint bind/connect/accept and peer facts, owner-only
Unix endpoint creation, TCP listener socket policy, and endpoint readiness
probing. Broker framing/routing, daemon retry/yield policy, and HTTP/protobuf
compatibility remain with the daemon. Additional native shutdown notifications
route through `process` into the daemon's neutral shutdown channel.

`ape` detects Actually Portable Executable (cosmocc) images by their magic
(`MZqFpD='`, `jartsr='`, `APEDBG='`) and, on Unix hosts, launches them as
`<loader> <image> <args...>`, because `posix_spawn` cannot exec an APE without a
`binfmt_misc` handler (NixOS ships none). Loader order: `FBUILD_APE_LOADER`, then
(Linux) the loader embedded in the image itself, then `ape` on PATH, then
`/usr/bin/ape` / `/usr/local/bin/ape`, then `/bin/sh` (which runs the image's
self-extracting prologue). Windows runs APE natively. Every
`subprocess::run_command*` spawn applies this; direct spawns build their command
with `process::command` / `process::tokio_command` instead of `Command::new`.

The embedded loader is located from the prologue's `dd skip=N count=M | gzip -dc`
line for the host CPU, inflated in-process, checked to be a 64-bit ELF for that
CPU, and installed content-addressed into the first owner-only (0700, not
group/world-writable, not a symlink), exec-capable directory among
`FBUILD_APE_CACHE_DIR`, `$XDG_CACHE_HOME`/`~/.cache` `fbuild/ape`,
`$XDG_RUNTIME_DIR/fbuild/ape`, and `$TMPDIR/fbuild-ape-<uid>`; failing all of
those, it lives in a sealed `memfd` exec'd via `/proc/self/fd/N`. The child needs
no PATH, `sh`, coreutils, `gzip`, `HOME` or `TMPDIR`. Installs are atomic
(temp file + `rename`), tampered or truncated copies are rewritten, and the
writes hold `process::exclusive_fork_guard` — a Go-style fork lock every spawn
helper holds shared across fork→exec — so no concurrently forked child can
inherit the writable descriptor and make the exec fail with `ETXTBSY`.
