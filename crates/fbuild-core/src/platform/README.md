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

`pty` owns the pseudo-terminal pair that handler tests use as a stand-in for
an attached board: `PtyPair::open` allocates master and slave on Linux,
`slave_name` reports the device path a test hands to fbuild as a serial port,
`wait_for_master_input` polls the master side, and `read_master_exact` drains
it. Holding the pair keeps the terminal live; dropping it closes both
directions, which is how the deploy-preemption tests make a port disappear.
Hosts without an openable pseudo-terminal report `is_supported() == false` and
`open()` returns `Unsupported`, so a test asks instead of selecting a host
itself.

`runtime` owns the runtime-library mechanics of third-party binaries fbuild
installs: `probe_binary` reports whether a binary can start and, when it
cannot, the linker's own complaint; `stage_runtime_libraries` places the
libraries a binary needs beside it; `runtime_library_dirs` names the
directories a child must have on its search path; and `library_path_var`
names the variable this host's loader reads, `None` when it resolves
libraries another way. Linux probes the binary (`LD_LIBRARY_PATH`) and stages
nothing; Windows stages `libiconv-2.dll` beside the executable and reports
`PATH` directories; macOS binaries are self-contained and need neither.
Which archive to install, which library to look for, and how to compose a
search path stay with the caller.

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
