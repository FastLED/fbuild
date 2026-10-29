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
