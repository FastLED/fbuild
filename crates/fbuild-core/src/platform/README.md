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
the loader embedded in the image itself (Linux ELF, macOS x86_64 Mach-O, or
Apple Silicon `ape-m1.c` compiled once with `cc`), then `ape` on PATH, then
`/usr/bin/ape` / `/usr/local/bin/ape`, then `/bin/sh` (which runs the image's
self-extracting prologue). Windows runs APE natively. Every
`subprocess::run_command*` spawn applies this; direct spawns build their command
with `process::command` / `process::tokio_command` instead of `Command::new`.

APE extraction, validation and installation are owned by `running-process`,
including the shared fork lock and transient `ETXTBSY` retry. fbuild's adapter
preserves `FBUILD_APE_LOADER` and selects loader storage from
`FBUILD_APE_CACHE_DIR`, `FBCACHE_DIR/ape`, `FBUILD_CACHE_DIR/ape`, the cache root
registered by the CLI/daemon, then the shared loader's host defaults. Embedded
zccache receives the corresponding canonical settings and launches the original
compiler directly through kernal-api 0.1.26; no fbuild native-path shim is needed.

See [`ape/README.md`](ape/README.md) for adapter and test locations.
