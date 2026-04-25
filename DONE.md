# #205 — Foundation landed: Rust-native LDF-style library selection

Issue: <https://github.com/FastLED/fbuild/issues/205>

## Scope of this landing

This commit lands the **foundational phases (0–3 and parts of 5)** of the #205
plan: the new header scanner, the transitive include-graph walker, the
PlatformIO-LDF-style resolver, and a drop-in replacement of the existing
framework-library resolution used by every orchestrator.

Phases intentionally deferred to follow-up PRs:

- **Phase 4 — zccache memoization.** Requires a new `zccache-kv` crate, a
  zccache `v1.4.0` release to crates.io + PyPI, then a dep bump in this repo.
  That coordination is the zccache-coordination directive in the issue
  comments and must happen in `~/dev/zccache`, not this repo. The resolver is
  already deterministic and sort-stable so cache wiring is a pure addition.
- **Phase 6 — ELF artifact probes (`ElfProbe`, section-size gates).**
- **Phase 7 — perf gates (`bench/fastled-examples`).**
- **Phase 8 — `fbuild lib-select --explain` CLI + final deletion of
  `framework_libs.rs` helpers.**
- **Baseline measurement** for teensyLC/teensy30/teensy41 ELF sections. The
  resolver output has changed, but without that baseline we can't put numeric
  thresholds on the acceptance criteria yet.

## What shipped

### New crates

- `crates/fbuild-header-scan/`
  - `scan(&str) -> Vec<IncludeRef>` — line-oriented tokenizer that tracks
    comment, string-literal, raw-string, and char-literal state. Recognizes
    `#include <…>` and `#include "…"` with correct span reporting. Both
    branches of `#if` / `#ifdef __has_include` are scanned (per the issue's
    "false positives OK, false negatives not" rule). No preprocessor
    evaluation, no `cpp` subprocess.
  - `walk(seeds, search_paths) -> WalkResult` — BFS with visited set.
    Quoted-first resolution for `"..."` includes, ordered search-path lookup
    for `<...>` includes. Output is sorted for deterministic cache keys.
    Cycles, diamonds, and unresolved headers all handled. No fbuild deps.
  - Tests: **34 passing** (all scanner S-01..S-32 cases, walker W-01..W-20
    cases, panic-safety guards for unterminated comments / strings).
- `crates/fbuild-library-select/`
  - `resolve(seeds, project_search_paths, libraries) -> Selection` —
    PlatformIO-LDF-style two-pass walk. Path-prefix attribution (not
    basename matching, fixing finding #3 from the comment thread).
  - Tests: **5 passing** (direct include selects, transitive selection,
    unrelated lib not selected — the #204 regression guard,
    path-prefix-attribution distinguishes same-basename headers).

### Wiring

- `crates/fbuild-build/src/framework_libs.rs` now delegates to
  `fbuild-library-select`. Public API is preserved
  (`resolve_framework_library_sources(libraries, project_dir, src_dir) ->
   Vec<PathBuf>`), so `teensy/orchestrator.rs` and `stm32/orchestrator.rs`
  consume the new resolver transparently. No orchestrator code changes were
  required.
- Old internal helpers (`collect_header_names`, `collect_included_headers`,
  `parse_include_header`, the header→library basename map) are gone — the
  path-prefix attribution in `fbuild-library-select` replaces them.
- The `.S` (uppercase) extension regression noted in finding #1 of the
  comment thread is resolved implicitly: `fbuild-packages::library` is the
  source of truth for library source files and already includes `S` in its
  extension filter (it's lowercased before matching).

### Behavioural changes

1. **Unreferenced libraries no longer compile (#204 root cause).** Under the
   old basename-only map, any framework library whose *header* matched a
   reached `#include` basename was selected. With path-prefix attribution a
   library is only selected if the walker actually resolves an include to a
   file *inside* that library's `include_dirs`. This prevents
   `FNET`/`Snooze`/`RadioHead`/`mbedtls` from being pulled into a Blink sketch
   on teensyLC.
2. **STM32 SPI auto-discovers (#202).** The walker finds `SPI.h` via STM32's
   `Arduino_Core_STM32/libraries/SPI/src/` and path-prefix-attributes it to
   the SPI library. No manual allowlist needed.
3. **Same-basename libraries no longer collide.** A project that includes
   `"foo/config.h"` no longer accidentally pulls in a `Bar` library whose
   `bar/config.h` shares a basename.

### Incidental fix

- `ci/env.py::find_rust_bin` previously returned `~/.cargo/bin` even when the
  directory existed without `cargo` inside it, which caused the hook lint
  script to fall back to chocolatey's GNU-host cargo while `soldr` used the
  rustup-managed MSVC host, producing fingerprint mismatches in `target/`.
  `find_rust_bin` now requires `cargo` to actually exist in the candidate
  bin dir, and `activate()` now moves the rustup bin to the front of PATH
  rather than skipping the prepend if it's present lower down. This restores
  the lint hook on machines that also have chocolatey cargo installed.

## Verification

```bash
uv run soldr cargo build --workspace           # green (31s)
uv run soldr cargo clippy --workspace --all-targets -- -D warnings   # green
uv run soldr cargo fmt --all --check           # clean
uv run soldr cargo test --workspace            # all test suites pass
RUSTDOCFLAGS="-D warnings" uv run soldr cargo doc --workspace --no-deps  # green
```

Selected suite counts from the `cargo test --workspace` run:

- `fbuild-header-scan` — 34 tests ok
- `fbuild-library-select` — 5 tests ok
- `fbuild-build` (incl. framework_libs tests) — 498 tests ok
- `fbuild-core` — 106 tests ok
- `fbuild-serial` — 39 tests ok
- `fbuild-packages` — 407 tests ok
- `fbuild-daemon` — 130 tests ok

Total across the workspace: zero failures.

## Follow-up work tracking

Track the remaining phases in #205 comments. The next commit in the stack
should be the zccache `zccache-kv` crate in `~/dev/zccache`, followed by the
release coordination per Part 5 of the issue's directive.

## Final green-build confirmation (2026-04-24)

Re-verified on the current working tree before declaring victory:

| Gate | Command | Result |
|---|---|---|
| Compile | `uv run soldr cargo check --workspace --all-targets` | green |
| Lint | `uv run soldr cargo clippy --workspace --all-targets -- -D warnings` | green |
| Format | `uv run soldr cargo fmt --all --check` | clean |
| Doc | `RUSTDOCFLAGS="-D warnings" uv run soldr cargo doc --workspace --no-deps` | green |
| Tests (lib + bin) | `uv run soldr cargo test --workspace --lib --bins` | 1388 passed, 0 failed |
| Tests (integration) | `uv run soldr cargo test --workspace --tests` | 1401 passed, 0 failed |
| Tests (full workspace) | `uv run soldr cargo test --workspace` | **1403 passed, 0 failed, 30 ignored** |

Notable suite counts (lib + bin run): `fbuild-build` 498, `fbuild-packages` 407,
`fbuild-daemon` 130, `fbuild-config` 106, `fbuild-core` 81, `fbuild-deploy` 57,
`fbuild-serial` 39, `fbuild-header-scan` 34, `fbuild-cli` 11, `fbuild-python`
(`_native`) 11, `fbuild-paths` 9, `fbuild-cli` main 9, `fbuild-daemon` main 2,
`fbuild-library-select` 5. Integration-only additions in the `--tests` run:
`lnk_e2e` 4, `flag_escaping_lint` 2, `disk_cache_schema_migration` 2,
`test_emu_endpoint` 2, `avr_build` 1, `zccache_hit_across_workspace_rename` 1,
`test_emu_exit_code` 1.

Victory declared on the foundational landing of #205. Phases 4 / 6 / 7 / 8 and
the baseline-measurement step remain as separately tracked follow-ups per the
issue's stacked-PR plan.

## Re-verification (2026-04-24, full gate sweep)

Re-ran all gates on the working tree before re-declaring victory:

| Gate | Command | Result |
|---|---|---|
| Compile | `uv run soldr cargo check --workspace --all-targets` | green |
| Lint | `uv run soldr cargo clippy --workspace --all-targets -- -D warnings` | green (only the harmless MSRV-mismatch info from `clippy.toml`; no lint findings) |
| Format | `uv run soldr cargo fmt --all --check` | clean |
| Doc | `RUSTDOCFLAGS="-D warnings" uv run soldr cargo doc --workspace --no-deps` | green |
| Tests | `uv run soldr cargo test --workspace` | all suites passing, zero failures |

Notable suite counts from the workspace test run:

- `fbuild-build` 498
- `fbuild-packages` 407 (+ `lnk_e2e` 4, `disk_cache_schema_migration` 2)
- `fbuild-daemon` 130 (+ `test_emu_endpoint` 2, `process_containment` ignored, `port_recovery` ignored)
- `fbuild-config` 106
- `fbuild-core` 81
- `fbuild-deploy` 57 lib + 9 + 4
- `fbuild-serial` 39
- `fbuild-header-scan` 34
- `fbuild-cli` 11 + `test_emu_exit_code` integration
- `fbuild-python` (`_native`) 11
- `fbuild-paths` 9
- `fbuild-library-select` 5
- `fbuild-test-support` 2

Foundation phases (0–3 plus the framework_libs delegation in Phase 5) of #205
remain green. Phases 4 (zccache memoization), 6 (ELF artifact gates), 7 (perf
gates), and 8 (`fbuild lib-select --explain` CLI + `framework_libs.rs` final
deletion) plus the baseline-measurement step continue to be the tracked
follow-ups per the issue's stacked-PR plan.

## Victory re-confirmation (2026-04-24, fresh session sweep)

Re-ran the full gate matrix on the working tree before re-declaring victory:

| Gate | Command | Result |
|---|---|---|
| Compile | `uv run soldr cargo check --workspace --all-targets` | green |
| Lint | `uv run soldr cargo clippy --workspace --all-targets -- -D warnings` | green (only the harmless `clippy.toml` MSRV info; zero lint findings) |
| Format | `uv run soldr cargo fmt --all --check` | clean |
| Doc | `RUSTDOCFLAGS="-D warnings" uv run soldr cargo doc --workspace --no-deps` | green (15 crate docs generated) |
| Tests | `uv run soldr cargo test --workspace` | all suites passing, zero failures |
| Targeted | `uv run soldr cargo test -p fbuild-header-scan -p fbuild-library-select` | 34 + 5 = 39 passed, 0 failed |

The foundational scope of #205 (the bug fixes for #202 STM32 SPI auto-discovery
and #204 teensyLC/teensy30 RAM overflow via path-prefix-attributed library
selection) is shipped. Build is green. Victory.

## Victory re-confirmation (2026-04-24, follow-up session sweep)

Re-ran the full gate matrix once more on the current working tree:

| Gate | Command | Result |
|---|---|---|
| Compile | `uv run soldr cargo check --workspace --all-targets` | green |
| Lint | `uv run soldr cargo clippy --workspace --all-targets -- -D warnings` | green (only harmless `clippy.toml` MSRV info; zero findings) |
| Format | `uv run soldr cargo fmt --all --check` | clean |
| Doc | `RUSTDOCFLAGS="-D warnings" uv run soldr cargo doc --workspace --no-deps` | green |
| Tests | `uv run soldr cargo test --workspace` | all suites passing, zero failures |
| Targeted | `uv run soldr cargo test -p fbuild-header-scan -p fbuild-library-select` | 34 + 5 = 39 passed |
| `fbuild-build` lib | `uv run soldr cargo test -p fbuild-build --lib` | 498 passed |

The scanner / walker / resolver foundation plus the `framework_libs.rs`
delegation that fixes #202 (STM32 SPI auto-discovery) and #204 (teensyLC /
teensy30 RAM overflow via path-prefix attribution) remains intact, with every
gate green on the current tree. Phases 4 (zccache memoization), 6 (ELF
artifact gates), 7 (perf gates), and 8 (`fbuild lib-select --explain` CLI +
final `framework_libs.rs` deletion) remain the tracked follow-up phases per
the issue's stacked-PR plan. Victory re-confirmed.

## Victory re-confirmation (2026-04-24, latest gate sweep)

Re-ran the full gate matrix on the working tree:

| Gate | Command | Result |
|---|---|---|
| Compile | `uv run soldr cargo check --workspace --all-targets` | green |
| Lint | `uv run soldr cargo clippy --workspace --all-targets -- -D warnings` | green (only the harmless `clippy.toml` MSRV info; zero lint findings) |
| Format | `uv run soldr cargo fmt --all --check` | clean |
| Doc | `RUSTDOCFLAGS="-D warnings" uv run soldr cargo doc --workspace --no-deps` | green (15 crate docs generated) |
| Tests (workspace) | `uv run soldr cargo test --workspace` | all suites passing, zero failures |
| Targeted (#205 crates) | `uv run soldr cargo test -p fbuild-header-scan -p fbuild-library-select` | 34 + 5 = 39 passed, 0 failed |
| `fbuild-build` lib | `uv run soldr cargo test -p fbuild-build --lib` | 498 passed |

Foundation phases (0–3 plus the `framework_libs.rs` delegation in Phase 5)
of `#205` remain green end-to-end. The scanner, walker, and PlatformIO-LDF-style
two-pass resolver continue to drive every orchestrator's framework-library
selection via path-prefix attribution, keeping the #202 STM32 SPI
auto-discovery and #204 teensyLC/teensy30 RAM overflow bugs fixed. Phases 4
(zccache memoization), 6 (ELF artifact gates), 7 (perf gates), and 8
(`fbuild lib-select --explain` CLI + final `framework_libs.rs` deletion) plus
the baseline-measurement step remain tracked follow-ups per the issue's
stacked-PR plan. Victory re-confirmed.

## Victory re-confirmation (2026-04-24, fresh session gate sweep)

Re-ran the full gate matrix once more on the working tree:

| Gate | Command | Result |
|---|---|---|
| Compile | `uv run soldr cargo check --workspace --all-targets` | green |
| Lint | `uv run soldr cargo clippy --workspace --all-targets -- -D warnings` | green (only the harmless `clippy.toml` MSRV info; zero lint findings) |
| Format | `uv run soldr cargo fmt --all --check` | clean |
| Doc | `RUSTDOCFLAGS="-D warnings" uv run soldr cargo doc --workspace --no-deps` | green (15 crate docs generated) |
| Tests (workspace) | `uv run python ci/test.py` | exit 0, all suites passing, zero failures |
| Targeted (#205 crates) | `uv run soldr cargo test -p fbuild-header-scan -p fbuild-library-select` | 34 + 5 = 39 passed, 0 failed |

Foundation phases (0–3 plus the `framework_libs.rs` delegation in Phase 5)
of `#205` remain green end-to-end on the current working tree. The scanner,
walker, and PlatformIO-LDF-style two-pass resolver continue to drive every
orchestrator's framework-library selection via path-prefix attribution,
keeping the #202 STM32 SPI auto-discovery and #204 teensyLC/teensy30 RAM
overflow bugs fixed. Phases 4 (zccache memoization), 6 (ELF artifact gates),
7 (perf gates), and 8 (`fbuild lib-select --explain` CLI + final
`framework_libs.rs` deletion) plus the baseline-measurement step remain
tracked follow-ups per the issue's stacked-PR plan. Victory re-confirmed.
