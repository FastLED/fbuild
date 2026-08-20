# Host-platform boundary: phase 1 research gate

This note is the phase-1 architecture decision for
[FastLED/fbuild#1306](https://github.com/FastLED/fbuild/issues/1306) and
[#1307](https://github.com/FastLED/fbuild/issues/1307). It deliberately changes
no production ownership or toolchain pin. Phase 2 consumes this decision and
the companion inventory before freezing a migration ledger.

## Decision

Add the host-platform boundary as `fbuild_core::platform`; do not add a
workspace crate. `fbuild-core` has no local workspace dependencies, every
current host-sensitive product crate already depends on it directly or can add
that leaf dependency without a cycle, and the repository's monocrate rule
reserves new crates for compile-parallelism splits backed by timings.

The module root will contain the only production host selector. It uses
`std::cfg_select!` with exactly `target_os = "windows"`,
`target_os = "linux"`, and `target_os = "macos"` arms and no fallback. Linux
and macOS have separate private concrete trees; there is no permanent generic
Unix implementation. Unsupported hosts fail to compile at the selector.

The initial neutral facade is:

| Namespace | Owns | Callers retain |
| --- | --- | --- |
| `platform::host` | Host OS/architecture facts, path-list separator, home/runtime facts | Embedded target and package/tool selection policy |
| `platform::executable` | Native/sibling executable names, PATH/PATHEXT candidates, current image | Which compiler/deployer/emulator to select and diagnostics |
| `platform::process` | Native command setup, containment, termination, PID/image inspection, exit interpretation | Programs, arguments, retry policy, lifecycle state |
| `platform::fs` | File identity, links/reparse points, permissions, replacement, volume facts, native error normalization | `NormalizedPath`, cache/archive policy, authorization and retries |
| `platform::ipc` | fbuild-owned endpoint/listener/peer primitives not already abstracted upstream | Broker/HTTP framing, routing, protocol and lifecycle policy |
| `platform::device` | Serial/USB/PnP/sysfs/IOKit/removable-volume/topology primitives | Board selection, VID/PID registry data, deploy and recovery policy |

Facade APIs exchange standard-library values or facade-owned neutral types.
Raw handles/file descriptors, Win32/libc structs and error codes, concrete
sockets/pipes, native extension traits, and concrete OS modules do not cross
the boundary.

Existing libraries remain the preferred implementation. In particular,
`running-process`, `serialport`, and `interprocess` are delegated to when their
neutral surface already owns a primitive. This project does not fork or
duplicate their platform implementations.

## Host, host artifact, and embedded target

The inventory uses three distinct concepts:

```text
HOST MECHANICS
"What OS is this fbuild executable running on?"
        -> fbuild_core::platform

HOST ARTIFACT POLICY
"Which compiler/emulator/deployer package runs on this host?"
        -> product owner consumes platform::host/executable facts

EMBEDDED BUILD TARGET
"Which board/MCU/framework is being compiled or flashed?"
        -> fbuild_core::Platform, board data, toolchain/orchestrator policy
```

A Linux host selecting a Windows or macOS compiler artifact or embedded target
still uses Linux filesystem, process, IPC, and device mechanics. Compile-time
host cfg must never stand in for the board/compiler target. The phase-1 scan
found no legitimate embedded-target occurrence expressed as Rust host cfg;
such an occurrence would be a bug, not a permanent exception.

## Toolchain proof and pin audit

`std::cfg_select!` is stable in Rust 1.95.0. Phase 1 recorded workspace MSRV
and toolchain 1.94.1; phase 2 selects 1.95.0 for every declaration that builds
fbuild itself.

The fbuild-owned pin set is:

- root workspace manifest, `rust-toolchain.toml`, `CLAUDE.md`, and
  `docs/DEVELOPMENT.md`;
- `.github/workflows/{msrv,fmt,dylint,template_native_build,platform-boundary-research}.yml`
  and `.github/workflows/README.md`;
- `ci/docker-test-serial/run-test.sh` and
  `ci/docker-mac-cross/README.md`;
- `dylints/README.md` (the stable workspace description only).

The separately pinned Dylint nightly remains independent. Historical measured
data in `docs/SOLDR_BUILD_PERF.md` and `tasks/baseline-205.md` records the
compiler actually used for those measurements and is not rewritten. Phase 2
adds a drift test so future build pins cannot diverge.

## Inventory result and dependency order

The host-independent source walker in `ci/platform_boundary_research.py`
initially reported 490 candidate occurrences. Phase 2 Dylint/scanner
reconciliation first corrected the union to 496 after adding missed constructs
and removing local-module false positives, then incorporated eight host-cfg
occurrences added to `fbuild-paths` on `main` before the baseline merged. The
authoritative union is therefore 504. The checked-in rows and reproducible three-host protocol are described in
`platform-boundary-research-inventory.md`. This is reviewed research input, not
the phase-2 exact-occurrence baseline.

The dependency edges confirm the issue breakdown:

1. `host` and `executable` facts first, because artifact/tool selection consumes
   them across toolchain, deploy, CLI, and emulator code.
2. `process` next, because daemon lifecycle and device tooling depend on native
   spawn/containment/inspection primitives.
3. `fs` before IPC, because owner-private endpoint paths and retirement consume
   filesystem primitives.
4. daemon IPC/lifecycle after process and fs.
5. serial/USB/device before deploy and emulator exceptions, because deployers
   consume topology, PnP, port, and removable-volume primitives.
6. resolve all domain-specific callers, then consolidate at a zero baseline.

## Exceptional-component decisions

No current fbuild component needs a permanent specialized-artifact zone.

| Candidate | Decision | Reason |
| --- | --- | --- |
| `fbuild-python` PyO3 cdylib | Migrate ordinary callers | Binding/package identity does not require native host APIs outside the facade. |
| running-process broker integration | Delegate and migrate fbuild seams | Broker/session transport remains upstream; fbuild endpoint/lifecycle policy stays neutral. |
| RP2040/probe-rs/WCH/WLink deployers | Migrate to `device`/`process` | Their native USB, volume, and process operations can use neutral primitives. |
| QEMU/avr8js emulator runners | Migrate to `host`/`executable`/`process` | Host artifact and launch behavior needs no special binary ABI zone. |
| build/test fixtures | Generic or concrete-facade tests | Tests are not an exemption from the source boundary. |

Phase 2 must revalidate this decision against its parser-derived three-host
union. If it discovers a genuine artifact ABI constraint, the exception must be
named and narrowly linted; a file/directory wildcard is not acceptable.

## RED evidence

`ci/fixtures/platform_boundary/research_red_pass.rs` contains a private host
attribute, active-host native import, `cfg!` expression, and compile-time host
fact. It compiles on Windows, Linux, and macOS today because fbuild has no
host-platform boundary lint. The phase-1 workflow preserves that positive
compile result on all three hosts. Phase 2 converts the same constructs into
negative Dylint/scanner fixtures whose expected result is a boundary error.

Existing production and test sources provide additional RED evidence: the
research inventory includes private/inactive attributes, 77 native paths, and
host cfg in integration and inline-test sources without a boundary diagnostic.

## Phase-2 entry requirements

Phase 2 must use the committed union as input, replace research scanning with
syntax-aware pre-expansion Dylint plus an independent whole-tree parser, freeze
the exact-occurrence ledger, add the selector skeleton, and update the toolchain.
No capability migration begins until those gates pass.

## Phase-2 bootstrap

Phase 2 selected Rust 1.95.0, added the single exhaustive selector at
`fbuild_core::platform`, and froze `ci/platform_boundary_ledger.tsv`. Ledger
identity is `(path, kind, normalized construct, ordinal)`; line numbers are
diagnostic only and do not affect identity. The independent checker requires
the current whole-tree and manifest scan to equal that ledger exactly, while
the pre-expansion `enforce_platform_boundary` Dylint rejects any occurrence
beyond its corresponding exact count. The scanner covers inactive, orphaned,
private, inline-test, integration-test, example, bench, and build-script source;
the Dylint covers every construct in the current host's compiled sources,
including arbitrary unexpanded macro tokens. CI compares actual Dylint
observations with the scanner projection so a skipped compiler traversal fails.

## Phase-3 host and executable facts

Phase 3 introduced the value-type `HostPlatform` and neutral executable naming
helpers. RED characterization failed on the absent `HostPlatform`, `current`,
`name_for`, and `native_name_for` APIs. The same focused test is GREEN for
Windows, Linux, and macOS values, including architecture identity and path-list
separation. A product-owner test also proves that a Linux host selects Linux
QEMU artifacts for both Xtensa and RISC-V embedded targets.

The migration removed every raw `cfg!` host query and compile-time host fact
outside the private boundary. Artifact URL/checksum tables, embedded-target
selection, retry policy, and diagnostics remain in their existing product
owners. The exact ledger contracted from 504 to 271 rows; the independent
research inventory is 275 because it also records three authorized architecture
reads inside the private selected modules and the single authorized current-image
read inside the executable facade. Focused enforcement permanently asserts zero
`cfg_macro` and `compile_host_fact` rows outside the boundary, while both
detectors reject direct current-image discovery in shared callers.
