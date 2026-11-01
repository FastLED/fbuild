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

Windows long-path registry mechanics also live behind `fs`: `long_path_support`
reads the system policy, `enable_long_paths` writes and verifies only that
policy, `legacy_path_units` measures native path units, and `short_cache_base`
supplies a local drive candidate without creating it. Interactive repair,
UAC authorization, short-cache selection, warnings, and daemon restarts belong
to callers. Non-Windows hosts report `NotApplicable`; repair and short-cache
base selection report `Unsupported`.

`ensure_private_directory` establishes the automatic short-cache hierarchy.
On Windows it verifies current-user ownership, rejects reparse points, creates
directories with protected user-only permissions, and holds ancestor handles
while traversing. The volume root is validated without changing its ACL.
Callers retain layout policy and must not use this operation to claim foreign
or explicitly configured cache directories.

`process` also owns resident-memory facts: `resident_memory(pid)` returns the
current resident set and, where the host tracks one, the peak. Linux reads
`VmRSS`/`VmHWM` from `/proc/<pid>/status`, macOS asks `proc_pidinfo` and takes
this process's peak from `getrusage`, and Windows calls `GetProcessMemoryInfo`.
A host that cannot answer returns `None`, never zero; formatting, sampling
cadence, and what a memory trend means stay with the daemon.

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

The embedded loader is located from the prologue's `dd skip=N count=M | gzip -dc`
line for the host CPU, inflated in-process, validated (64-bit ELF or Mach-O for
that CPU), and installed content-addressed into the first owner-only (0700, not
group/world-writable, not a symlink), exec-capable directory among
`FBUILD_APE_CACHE_DIR`, fbuild's cache root (registered by the daemon and CLI via
`ape::set_default_cache_root`), `$XDG_CACHE_HOME`/`~/.cache` `fbuild/ape` (macOS:
`~/Library/Caches/fbuild/ape`) and `$XDG_RUNTIME_DIR/fbuild/ape`; on Linux, failing
all of those, it lives in a sealed `memfd` exec'd via `/proc/self/fd/N`. The child
needs no PATH, `sh`, coreutils, `gzip`, `HOME` or `TMPDIR`, and the loader is put
first on its PATH as `ape` so APE programs it spawns itself resolve too. Installs
are atomic (temp file + `rename`), tampered or truncated copies are rewritten,
and the writes hold `process::exclusive_fork_guard` — a Go-style fork lock every
spawn helper (and the few allowlisted direct spawns, via
`process::shared_fork_guard`) holds shared across fork→exec — so no concurrently
forked child can inherit the writable descriptor. Spawners fbuild doesn't
control (zccache) can still race it, so APE launches retry `ETXTBSY` briefly.
