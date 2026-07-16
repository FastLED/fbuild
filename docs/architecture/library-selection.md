# Library selection (LDF-style)

> Status: foundation phases (0–3 + Phase 5 framework_libs delegation) landed
> in PR #207. Phase 6 acceptance gates and Phase 8.a `lib-select` CLI landed
> in PR #208. Phase 4 (zccache memoization, `resolve_cached`) shipped in
> PR #212 and is wired into the teensy + stm32 orchestrators by #214.
> Phase 7 perf gates remain a follow-up in `#205`.

## Why

PlatformIO's LDF picks the right libraries for a sketch but is slow (Python
+ single-threaded + SCons graph overhead). fbuild's earlier basename-matching
helper produced wrong selections under #202 (STM32 SPI not auto-discovered)
and #204 (teensyLC RAM overflow from FNET / Snooze / RadioHead / mbedtls
being wrongly compiled). This subsystem replaces that helper with a
PlatformIO-LDF-faithful, Rust-native, deterministic resolver that orchestrators
call transparently through `fbuild-build::framework_libs`.

## What

Three crates form the subsystem:

- `fbuild-header-scan::scan` — line-oriented C/C++ tokenizer that emits
  `IncludeRef` per `#include`. Pure function, no I/O. Tracks comment,
  string-literal, raw-string, and char-literal state. Both branches of
  `#if` / `#ifdef` are scanned (false positives are acceptable, false
  negatives are not).
- `fbuild-header-scan::walk` — BFS over the include graph. Quoted-first
  resolution for `"..."`, ordered search-path lookup for `<...>`. Visited
  set guards cycles. Output is canonicalized and sorted for deterministic
  cache keys.
- `fbuild-library-select::resolve` — PlatformIO-LDF-style two-pass walk:
  1. From project seeds, BFS marks libs whose `include_dirs` contain a
     reached path (path-prefix attribution).
  2. Reconciliation: for each selected lib, re-walk seeded with its full
     source set; libs newly reached in pass 2 are also marked.
  Output `Selection` is sorted-by-name and deduplicated.

`fbuild-build::framework_libs` is the integration glue — orchestrators
(`teensy/orchestrator.rs`, `stm32/orchestrator.rs`, ...) call
`resolve_framework_library_sources` transparently with no orchestrator-side
code changes.

## Sequence

```text
project translation units       framework libraries
(sketch, src/)                  (e.g. Arduino_Core_STM32/libraries/*)
        │                              │
        │ collect_project_seeds        │ FrameworkLibrary { name,
        ▼                              │   include_dirs, source_files }
   seeds: Vec<PathBuf>                 │
        │                              │
        └────────────┬─────────────────┘
                     ▼
        fbuild-library-select::resolve
          ├─ pass 1: walk(seeds, project + lib include dirs)
          │            └─ for each reached path:
          │               attribute by include_dirs prefix → mark lib
          ├─ pass 2: for each marked lib, walk(lib.source_files, ...)
          │            └─ newly reached paths attribute new libs
          └─ Selection { included_files, required_libraries,
                         source_files, include_dirs, unresolved }
                     │
                     ▼
        fbuild-build::framework_libs
          flatten Selection.source_files → Vec<PathBuf>
                     │
                     ▼
        orchestrator compile list
```

## Why path-prefix attribution

PlatformIO LDF's `search_deps_recursive` (piolib.py:428) attributes
resolved paths to libs by *include_dirs prefix*, not basename. fbuild does
the same. Concrete consequences:

- A project including `"foo/config.h"` will not pull in a `Bar` library
  whose `bar/config.h` shares a basename. (Closed: misattribution risk.)
- A library is selected only when the walker actually resolves an include
  *into* its `include_dirs`. (#204: FNET / Snooze / RadioHead / mbedtls
  no longer pulled in for a Blink sketch on teensyLC.)
- STM32 `SPI.h` resolves through `Arduino_Core_STM32/libraries/SPI/src/`
  and prefix-attributes to the SPI library — no manual allowlist needed
  (#202).

## Rooted at the sketch

Only project translation units seed the include walk. Headers under a local
`lib/` tree are searched normally, but are never independent roots: a local
library header must be reached from the sketch's transitive include graph
before its framework dependencies can be selected. This prevents an inactive
header anywhere in a large library from self-selecting an unrelated framework
library (FastLED/fbuild#1094).

## Why two-pass (not fixed-point)

PlatformIO `chain` mode runs BFS from project sources, then ONE
reconciliation pass that re-seeds with each dependent lib's full source set
(piolib.py:1156). Unconverged deps drop silently (L1164–L1167). The
original issue framing ("fixed-point over include closure — typically 2–3
iterations") was wrong; we match PIO's 2-pass semantics exactly so users
who flip between PlatformIO and fbuild see the same library set.

## Cache key

`resolve_cached` (see `crates/fbuild-library-select/src/cache.rs`) hashes:

- sorted blake3s of project source content,
- sorted blake3s of each lib's canonical header
  (`<include_dir>/<lib_name>.h`),
- ordered search-path list (order matters — PIO's resolution is
  order-sensitive),
- toolchain triple,
- framework install path + version identifier,
- `SCANNER_VERSION` (bumped on tokenizer changes),
- `LDF_MODE_VERSION` (bumped on resolver semantic changes).

The KvStore is opened lazily by `framework_libs::library_select_kv_store()`
under `fbuild_paths::get_cache_root().join("library-selection")`, which
respects `FBUILD_DEV_MODE` and `FBUILD_CACHE_DIR`. Wiring the cache was a
pure addition: orchestrators that hit a backend error fall through to the
uncached `resolve(...)` rather than fail.

## Determinism

Walker output is sorted (`BTreeSet` → `Vec`). Resolver output is sorted by
lib name and deduplicated, and `included_files`, `source_files`, and
`include_dirs` are all sorted-and-deduped before return. Same inputs
produce byte-equal `Selection` output, which is what makes Phase 4 cache
keys safe.

## Tests

- 34 scanner tests (`crates/fbuild-header-scan/src/scanner.rs`) covering
  S-01..S-32 plus panic-safety guards for unterminated comments and strings.
- Walker tests in `walker.rs` (W-01..W-20: resolution order, cycle and
  diamond termination, deterministic output ordering, unresolved-include
  reporting).
- Resolver tests in `crates/fbuild-library-select/src/lib.rs` including
  the #204 regression guard, sort-stability, missing-include-dir
  tolerance, and same-basename-different-library disambiguation.
- Acceptance gates for AC#1 (teensyLC) and AC#4 (stm32 SPI
  auto-discovery) live in `crates/fbuild-build/tests/`
  (`teensylc_acceptance.rs`, `stm32_acceptance.rs`). They are
  `#[ignore]`'d by default and run by CI with `--ignored`. They use
  `fbuild-test-support`'s `ElfProbe` and `CompileDb` utilities to probe
  the produced firmware.

## Future work

- **Phase 7** — perf gates wired into `bench/fastled-examples` to catch
  regressions in both cold-resolve and warm cache-hit paths (#215).
- **Phase 8.b** — `framework_libs.rs` is now a thin delegator: scan-root
  collection, the uncached and cached entry points, the `KvStore` opener,
  and tests. No remaining dead helpers to retire.

## References

- PlatformIO LDF source: `platformio/builder/tools/piolib.py`.
- Issue: FastLED/fbuild#205.
- Closes: FastLED/fbuild#202, FastLED/fbuild#204.
- Cache wiring: FastLED/fbuild#212 (cache helper), FastLED/fbuild#214
  (orchestrator wiring).
