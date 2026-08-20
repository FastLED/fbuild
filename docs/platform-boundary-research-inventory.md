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
`unix` had been mistaken for native crates. The corrected, authoritative union
contains **496 rows**:

| Kind | Rows |
| --- | ---: |
| `attr_cfg` | 194 |
| `cfg_macro` | 198 |
| `compile_host_fact` | 14 |
| `native_path` | 77 |
| `native_dependency` | 7 |
| `target_dependency_table` | 6 |

| Classification | Rows |
| --- | ---: |
| Host mechanic | 390 |
| Host artifact policy | 106 |
| Embedded build-target policy | 0 |
| Specialized artifact | 0 |

| Capability | Rows |
| --- | ---: |
| `host_executable` | 117 |
| `device` | 117 |
| `process` | 110 |
| `host` | 97 |
| `fs` | 43 |
| `ipc` | 12 |

## Distribution by crate

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
| `fbuild-paths` | 9 |
| `fbuild-packages-fetch` | 9 |
| `fbuild-python` | 4 |
| `fbuild-build-engine` | 4 |
| `fbuild-config` | 4 |
| `fbuild-build-arm` | 3 |
| `fbuild-build-esp` | 2 |

The 496-row count is larger than #1306's preliminary 386 matching lines because
this scan also records compile-time host facts, native paths/dependencies, and
target-specific dependency tables and treats multiple constructs on a line as
separate findings.

## Manifest findings

Target-specific native ownership currently exists in:

- `fbuild-core`: Unix `libc`;
- `fbuild-cli`: Windows `windows-sys`;
- `fbuild-deploy`: Windows `windows-sys`;
- `fbuild-serial`: Windows `windows-sys`;
- `fbuild-daemon`: Unix `libc`, including a dev dependency, plus
  cross-platform `interprocess`.

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
