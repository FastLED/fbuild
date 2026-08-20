# Phase-1 host-platform research inventory

This document records the reproducible inventory for
[FastLED/fbuild#1307](https://github.com/FastLED/fbuild/issues/1307). The rows in
`ci/platform_boundary_research.tsv` are reviewed input to phase 2, not a
grandfathering baseline.

## Scope and method

The scanner walks every handwritten `.rs` file below `crates/`, including crate
roots, inline tests, integration tests, examples, benches, and otherwise
inactive module files. It strips comments and quoted string contents while
preserving offsets, then records host cfg attributes/macros, compile-time host
facts, native paths, concrete platform references, target dependency tables,
and native dependencies. Generated output, `target/`, Dylint fixtures, and
external/vendor sources are outside this research union.

Because the scanner walks source rather than expanded modules, the same checkout
produces the same union on every host. `.github/workflows/platform-boundary-research.yml`
runs the drift check and fixture tests on Windows, Linux, and macOS and prints a
host-labelled total. Phase 2 must still reconcile the three raw parser/Dylint
inventories before it freezes its authoritative ledger; compiler hooks alone
cannot see wrong-host or orphaned module files.

Reproduce locally with:

```powershell
uv run --no-project python ci/platform_boundary_research.py --check --print-totals
uv run --no-project python -m unittest ci.test_platform_boundary_research
```

## Reconciled union

Phase 1 initially reported 490 rows. Phase 2's Dylint/scanner reconciliation
found that a Rust character literal containing `"` hid one later `cfg!` from
the research lexer and that concrete local `windows::...` module paths were
not included; the same reconciliation also added concrete
`interprocess::local_socket` transports. A subsequent exact AST comparison
removed 25 false positives where local modules named `linux`, `macos`, or
`unix` had been mistaken for native crates. Eight host-cfg occurrences added
to `fbuild-paths` on `main` before the phase 2 baseline was merged were then
reconciled into the ledger. The phase-2 bootstrap's corrected, authoritative
union contained **504 rows**:

| Kind | Rows |
| --- | ---: |
| `attr_cfg` | 202 |
| `cfg_macro` | 198 |
| `compile_host_fact` | 14 |
| `native_path` | 77 |
| `native_dependency` | 7 |
| `target_dependency_table` | 6 |

| Classification | Rows |
| --- | ---: |
| Host mechanic | 398 |
| Host artifact policy | 106 |
| Embedded build-target policy | 0 |
| Specialized artifact | 0 |

| Capability | Rows |
| --- | ---: |
| `host_executable` | 117 |
| `device` | 117 |
| `process` | 110 |
| `host` | 105 |
| `fs` | 43 |
| `ipc` | 12 |

## Phase-2 distribution by crate

| Crate | Rows |
| --- | ---: |
| `fbuild-core` | 101 |
| `fbuild-toolchain` | 101 |
| `fbuild-deploy` | 76 |
| `fbuild-daemon` | 66 |
| `fbuild-serial` | 47 |
| `fbuild-cli` | 47 |
| `fbuild-library` | 13 |
| `fbuild-build` | 10 |
| `fbuild-paths` | 17 |
| `fbuild-packages-fetch` | 9 |
| `fbuild-python` | 4 |
| `fbuild-build-engine` | 4 |
| `fbuild-config` | 4 |
| `fbuild-build-arm` | 3 |
| `fbuild-build-esp` | 2 |

The 504-row count is larger than #1306's preliminary 386 matching lines because
this scan also records compile-time host facts, native paths/dependencies, and
target-specific dependency tables and treats multiple constructs on a line as
separate findings.

## Phase-3 host-fact contraction

Phase 3 replaced every raw `cfg!` OS/architecture query and compile-time host
fact outside the boundary with `fbuild_core::platform::{host,executable}`.
Product owners retain embedded-target and artifact-table policy; they now
consume an explicit neutral `HostPlatform`. The exact enforcement ledger fell
from **504 to 271 rows**, deleting 233 migrated occurrences. Its current shape
is:

| Kind | Rows |
| --- | ---: |
| `attr_cfg` | 181 |
| `native_path` | 77 |
| `native_dependency` | 7 |
| `target_dependency_table` | 6 |
| `cfg_macro` | 0 |
| `compile_host_fact` | 0 |

| Classification | Rows |
| --- | ---: |
| Host mechanic | 262 |
| Host artifact policy | 9 |

| Capability | Rows |
| --- | ---: |
| `process` | 95 |
| `device` | 76 |
| `host` | 36 |
| `fs` | 39 |
| `host_executable` | 20 |
| `ipc` | 5 |

The host-independent research inventory contains 275 rows because it also
records the three authorized `std::env::consts::ARCH` reads in the private
Windows, Linux, and macOS implementations and the single authorized
`std::env::current_exe` read inside the executable facade. Those rows are
intentionally absent from the enforcement ledger and Dylint baseline;
regression tests verify that boundary implementation findings cannot be
grandfathered while direct shared-caller current-image reads are rejected.

## Phase-4 process contraction

Phase 4 moved fbuild-owned spawning, containment, PID/image inspection,
termination, waiting, and exit interpretation behind `platform::process`.
Compatible contained and detached spawning delegates to the already pinned
`running-process`; product callers continue to own their retry, escalation,
diagnostic, and lifecycle policy.

The exact enforcement ledger fell from **271 to 162 rows**, deleting 109
migrated occurrences. Its current shape is:

| Kind | Rows |
| --- | ---: |
| `attr_cfg` | 119 |
| `native_path` | 36 |
| `native_dependency` | 4 |
| `target_dependency_table` | 3 |
| `cfg_macro` | 0 |
| `compile_host_fact` | 0 |

| Classification | Rows |
| --- | ---: |
| Host mechanic | 157 |
| Host artifact policy | 5 |

| Capability | Rows |
| --- | ---: |
| `device` | 76 |
| `fs` | 35 |
| `host` | 22 |
| `host_executable` | 13 |
| `process` | 14 |
| `ipc` | 2 |

The host-independent research inventory contains 171 rows: the 162 enforced
caller occurrences plus nine authorized facade/private-implementation
occurrences. Its process capability has contracted from 95 to 17 rows; three
of those are authorized implementation details, leaving 14 exact caller rows
for later IPC/device capability phases.

## Phase-5 filesystem contraction

Phase 5 moved native path and file identity, extended-prefix handling,
permissions, directory links and reparse points, volume facts, error
classification, shared output-file opening, atomic replacement, and blocked-I/O
retirement behind `platform::fs`. Neutral product owners retain
cache/archive/authorization/diagnostic/lock/retry policy.

The exact enforcement ledger fell from **162 to 107 rows**, deleting 55
migrated occurrences. Its current shape is:

| Kind | Rows |
| --- | ---: |
| `attr_cfg` | 81 |
| `native_path` | 21 |
| `native_dependency` | 3 |
| `target_dependency_table` | 2 |
| `cfg_macro` | 0 |
| `compile_host_fact` | 0 |

| Classification | Rows |
| --- | ---: |
| Host mechanic | 104 |
| Host artifact policy | 3 |

| Capability | Rows |
| --- | ---: |
| `device` | 62 |
| `host` | 20 |
| `process` | 12 |
| `host_executable` | 11 |
| `ipc` | 2 |
| `fs` | 0 |

The normalized Dylint projection is 103 rows. The host-independent research
inventory contains 137 rows: 107 enforced caller occurrences plus 30 exact
authorized facade/private-implementation occurrences. Ten of those are native
filesystem implementation or dependency details.

## Manifest findings

Target-specific native ownership currently exists in:

- `fbuild-core`: Unix `libc` and Windows `windows-sys`, confined to selected filesystem implementations;
- `fbuild-cli`: Windows `windows-sys`;
- `fbuild-serial`: Windows `windows-sys`;
- `fbuild-daemon`: cross-platform `interprocess`.

Phase 2's manifest checker must freeze exact occurrences. Later capability
phases move native dependency ownership into `fbuild-core`'s private concrete
platform implementation and remove caller target tables when their final use is
migrated.

## Classification limits

Phase 1 classifies by source ownership and reviewed subsystem responsibility.
It intentionally does not claim that a lightweight source walker is an AST
authority. In particular, phase 2 must normalize nested cfg token trees,
aliases, raw strings, grouped/glob imports, raw handle/fd traits, and repeated
identical occurrences with stable ordinals. If the phase-2 union differs, its PR
must explain every delta instead of regenerating a baseline to make CI pass.
