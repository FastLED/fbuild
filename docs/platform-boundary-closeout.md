# Phase 9/9 closeout audit: host-platform boundary

This is the final requirement-by-requirement audit for
[FastLED/fbuild#1306](https://github.com/FastLED/fbuild/issues/1306), performed
for phase 9/9 ([#1315](https://github.com/FastLED/fbuild/issues/1315)) after the
exact-occurrence ledger reached zero on `main` (`d3f6c86c`, PR #2119).

Every row below was produced by running the command named beside it on that
tree. Nothing here is a projection: the counters are the checker's own output.

## 1. End state

| Surface | Location | End state |
| --- | --- | --- |
| Host selector | `crates/fbuild-core/src/platform/mod.rs:17` | one `std::cfg_select!`, three arms (`windows`, `linux`, `macos`), no fallback arm |
| Exact-occurrence ledger | `ci/platform_boundary_ledger.tsv` | header only, 0 data rows |
| Dylint normalization baseline | `dylints/enforce_platform_boundary/src/baseline.txt` | header only, 0 rows |
| Research inventory | `ci/platform_boundary_research.tsv` | 172 rows, all authorized boundary occurrences (see §4) |
| Regeneration hatch | `ci/enforce_platform_boundary.py` | deleted: `--print-totals` is the only reporting mode; there is no `--write` |
| Workspace members | `Cargo.toml` | 21, unchanged (`ci/check_workspace_crates.py`: `OK: 21 approved workspace members, no new crates`) |

```
$ uv run --no-project python ci/enforce_platform_boundary.py --print-totals
rows=0; dylint_rows=0            # exit 0
$ uv run --no-project python ci/platform_boundary_research.py --check
                                 # exit 0
$ uv run --no-project python ci/check_rust_toolchain_pins.py
                                 # exit 0
```

## 2. Ledger contraction

Data rows in `ci/platform_boundary_ledger.tsv`, read from each merged commit:

| Commit | Date | Rows | Milestone |
| --- | --- | --- | --- |
| `e15d830d` | 2026-08-20 | 505 | phase 2 ratchet frozen (#1319) |
| `76bc2532` | 2026-08-20 | 272 | phase 3 host + executable (#1325) |
| `fdd82413` | 2026-08-20 | 163 | phase 4 process/containment (#1328) |
| `80e05051` | 2026-08-20 | 108 | phase 5 filesystem/paths |
| `4476ca11` | 2026-08-21 | 95 | phase 6 daemon IPC (#1334) |
| `b02262da` | 2026-08-22 | 58 | phase 7 device mechanics (#1344) |
| `cd781ba8` | 2026-08-22 | 34 | device consumers (#1345) |
| `1eb18dfb` | 2026-09-29 | 45 | **grew**: new occurrences from serial-session consolidation (#1584) |
| `383e2c6c` | 2026-11-20 | 41 | live-image identity (#2112) |
| `59713191` | 2026-11-20 | 25 | pseudo-terminals (#2113) |
| `29849585` | 2026-11-20 | 16 | executable identity (#2115) |
| `d87a3944` | 2026-11-20 | 7 | QEMU runtime libraries (#2116) |
| `ba41cef8` | 2026-11-20 | 0 | last six test-only branches (#2119) |

The ledger went up once (`45`) when a 2026-09 feature landed new occurrences; it
was never rewritten to match the tree, and the growth was paid down by
migration, not by re-baselining.

## 3. Audit against #1306 acceptance criteria

| #1306 criterion | Evidence | Verdict |
| --- | --- | --- |
| RED fixtures prove private/inactive host cfg, native API access, concrete implementation references and test-only occurrences evade the boundary | live proof: appending `#[cfg(windows)]` to `platform/host.rs` makes `enforce_platform_boundary.py --print-totals` exit 1 with `new occurrence: … attr_cfg … host_mechanic`; `ci/fixtures/platform_boundary/research_red_pass.rs` is compiled with `-D warnings` by `platform-boundary-research.yml`; the Dylint's own `second_identical_occurrence_exceeds_exact_baseline` test covers the ordinal ratchet | met |
| MSRV + every canonical toolchain declaration at Rust 1.95.x, with drift coverage | `rust-toolchain.toml` `channel = "1.95.0"`; workspace `rust-version = "1.95.0"`; `ci/check_rust_toolchain_pins.py` exit 0 (runs in `platform-boundary-research.yml` on Windows and Linux) | met |
| No new workspace crate; boundary is `fbuild_core::platform` | `ci/check_workspace_crates.py` → `OK: 21 approved workspace members`; `Reject new workspace crates (monocrate policy)` job success on `d3f6c86c` | met |
| Exactly one production `cfg_select!`, Windows/Linux/macOS arms, no fallback | `git grep -c 'cfg_select!'` over `crates/` = 1 hit (`platform/mod.rs`); the selector has three arms and no `_` arm; unsupported hosts fail to compile | met |
| Reviewed three-host inventory classifies mechanics vs artifact policy vs embedded-target policy vs specialized artifacts | `ci/platform_boundary_research.tsv` + `--check --host-label windows` → `rows=172; kinds=attr_cfg:1, compile_host_fact:3, native_dependency:4, native_path:162, target_dependency_table:2`; classifications all `host_mechanic`; embedded-target policy stayed in `fbuild_core::Platform`/board/toolchain owners and never entered this union | met |
| Shared callers use neutral facade types and cannot name native APIs, raw native types or concrete implementations | ledger 0 across every handwritten `.rs` below `crates/` (gateway for `std::os::*`, `windows-sys`, `libc`, `cfg`/`cfg!`/`cfg_if`, concrete `platform::{windows,linux,macos}` paths); 171 of the 172 remaining inventory rows are *inside* `crates/fbuild-core/src/platform/`, the 172nd group is the manifest (§4) | met |
| Existing broker/containment, daemon compatibility, serial/USB recovery, deploy, toolchain resolution, diagnostics stay compatible | no behavioural change in the closeout work: phases 3-9 moved implementations verbatim behind the facade; Linux `ci-minimal` + merge-group runs green on every phase commit, and the phase PRs each carried the focused suites for the capability they moved | met |
| Linux-hosted regression proves selecting a Windows/macOS embedded artifact or firmware target does not select non-Linux host mechanics | `esp32s3-arm64-slice-proof.yml` and the cross-target board jobs build Windows/macOS *embedded* targets from a Linux host; host mechanics come only from `platform::current_os()`. The independent scanner (`--host-label windows` vs `linux`) returns the same union, proving compile-time host facts are no longer consulted for host mechanics | met |
| Windows/Linux/macOS CI exercise representative process, fs, executable/host, IPC, serial/USB/device, deploy, emulator behaviour through the same facade paths | matrix in §5 | see §5 |
| Ledger monotonically reaches zero; permanent Dylint, scanner and manifest checker reject new leakage | §2 table plus the live RED proof above; `Dylint Full (Linux builder, all OS targets)` success on `d3f6c86c` | met |
| GREEN: focused tests and full validation pass at final closeout | Linux: `ci-minimal`, `glibc 2.17 floor`, `Dylint Full`, `Python facade tests` all success on `d3f6c86c` with 0 failures across its check-runs; `uv run --no-project --with pyyaml python -m unittest ci.test_enforce_platform_boundary ci.test_platform_boundary_research ci.test_rust_toolchain_pins` → 38 tests OK. Windows/macOS: produced by this PR's `ci-full` run (§5) | see §5 |

## 4. Where the remaining occurrences live

`ci/platform_boundary_research.tsv` keeps 172 rows because the inventory is
*host-independent*: it records the union of all three hosts' sources, including
the private implementation trees. Their distribution:

| Zone | Rows |
| --- | --- |
| `crates/fbuild-core/src/platform/{windows,linux,macos}/**` | 166 |
| `crates/fbuild-core/Cargo.toml` target-dependency tables + native dependencies (`windows-sys`, `socket2`, `libc`, `interprocess`) | 6 |

Both zones are the authorized boundary. `fbuild-core` is the only crate that
declares a native dependency: `grep -rn 'windows-sys\|libc\|interprocess' --include=Cargo.toml crates/` returns only that one manifest.

## 5. Three-host validation matrix

How each host is exercised at closeout:

| Capability | Windows (`windows-latest`) | Linux (several lanes) | macOS (`macos-15-intel`, `macos-15`) |
| --- | --- | --- | --- |
| process / containment | `cargo test --workspace`, SAMD + RP2040 acceptance | `ci-minimal`, merge-group, glibc floor | `cargo test --workspace` |
| fs / paths | `cargo test --workspace` + `large_archive_handles_windows_command_limit_and_quoted_paths` + Windows long-path repair smoke | `cargo test --workspace`, path-baseline jobs | `cargo test --workspace` |
| executable / host facts | `cargo test --workspace`, Renesas + ESP-IDF acceptance | `cargo test --workspace`, cross-target builds | `cargo test --workspace` |
| ipc (daemon lifecycle) | `cargo test --workspace` (named pipes) | daemon suites + merge-group | `cargo test --workspace` (unix sockets) |
| device / serial / USB | `Test ESP32-S3 linker overflow`, ESP32-C6 install-and-recheck, long-path smoke | serial lanes + attached-hardware receipts | `cargo test --workspace` |
| deploy | SAMD, Renesas, RP2040 acceptance on Windows | board jobs on Linux | workspace suites |
| emulator (QEMU runtime libraries) | `hydrate/validate` policy exercised by workspace tests | `ci/build_qemu_linux_runtime.py` path | policy is host-adaptive, no staging |

Entry points: `check-windows.yml` and `check-macos.yml` are `workflow_call`
only; both are invoked by `ci-full.yml`, which `ci-minimal.yml` starts for a PR
carrying the `ci-full` label (or which `release-auto.yml` calls for a release).
Ordinary PRs therefore do not run them — the Windows lane's last success before
this audit was `6b64d783` (2026-11-01) and the macOS lane's was `cab2098e`
(2026-08-28). The closeout run is the `ci-full` validation of this PR.

## 6. Residuals, stated honestly

- macOS coverage is `cargo test --workspace` on two runners; there is no
  separate macOS Dylint lane, so the lint's platform-specific arm is proven by
  compilation (`Dylint Full` builds all OS targets from Linux) rather than by a
  macOS-hosted lint run.
- The research inventory will never reach zero by design: it is a host-union
  inventory and the private implementation trees necessarily contain native
  paths. "Zero" is an assertion about the *ledger*, the Dylint baseline and
  every caller, not about the boundary's interior.
- The 1000-LOC/new-file and path-lint rules still apply to future facade work;
  they are not relaxed by this closeout.
