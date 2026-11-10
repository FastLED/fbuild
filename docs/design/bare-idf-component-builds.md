# Bare ESP-IDF / CMake component builds — research, decision and delivered implementation

**Status:** research complete 2026-11-09, **delivered 2026-11-10** (PRs #1975-#1987;
follow-ups #1970-#1974 all merged) · **Issue:** FastLED/fbuild#1761 (parent #1664;
the *reject* half landed in #1730) · **Dates:** research 2026-11-09, delivery 2026-11-10

fbuild used to build ESP32 projects only through the Arduino core and its
precompiled ESP-IDF libraries, so a bare `framework = espidf` project was
rejected rather than silently producing an Arduino-only build (#1730). This note
compares the options with measured evidence (the spike below), recommends
option 3, and records the delivered implementation: an **opt-in** delegating
path that builds the project's own `idf.py`/CMake project with a pinned ESP-IDF
release and publishes the artifacts as the proof. Every number here is measured
on the steward host; the delivery table says where each piece lives.

## Recommendation

**Option 3 (hybrid), opt-in, proof-gated.** Keep the native Arduino-on-IDF path
exactly as it is, and add a delegating path that runs the project's own
`idf.py`/CMake build against a pinned ESP-IDF release, then records the
artifacts it produced. Do not implement the native component model (option 2):
the measured cost of delegation is bounded and the cost of re-implementing
IDF's build layer is unbounded.

The delegation must be **explicit** (the delivered opt-in is
`board_build.build_system = espidf`), because it changes the caching story,
the install footprint (~1.8 GB per release per chip family, measured below)
and the meaning of "cold build".

## What exists today (after delivery)

Two paths, one per-environment switch, and the native one is unchanged.

- **Native (the default).** pioarduino `platform-espressif32` +
  `framework-arduinoespressif32`: fbuild compiles sketch, cores and libraries
  itself and links the precompiled IDF archives
  (`crates/fbuild-build-esp/src/esp32/esp32_linker.rs`). Untouched by this work.
- **Delegated (`board_build.build_system = espidf`).** The project's own
  `CMakeLists.txt` is built by the pinned ESP-IDF v5.5.1 (`fcae3288`) that
  fbuild installs as packages —
  `crates/fbuild-build-esp/src/esp32/espidf_delegate.rs`, entered from
  `orchestrator/build.rs` after `prepare_build_system`. `native` and `arduino`
  mean the native path; an unknown value is a configuration error that names the
  valid ones instead of silently picking one.
- **The artifacts are the proof.** `espidf_artifacts.rs` derives the layout from
  the `flasher_args.json` the IDF build itself writes — no second copy of IDF's
  layout knowledge — and publishes the app bin, ELF, map, bootloader, partition
  table, generated `sdkconfig` and `compile_commands.json` flat under
  `<build>/idf/`, indexed in `idf-artifacts.json` with byte counts and
  `"cache": "cold-only"`.
- **One compile database.** `espidf_compile_db.rs` re-roots and merges the
  delegated database into `CompileDatabase::expected_output_path`, the location
  the native path already publishes to.
- **A component build cannot be claimed without proof** — `IdfCmakeProof`, see
  "Metadata / false-coverage guard" below.
- **Caching is declared cold-only**, with the measurement in this note.
- **The reject half still holds.** Without the opt-in, a bare
  `framework = espidf` fails fast (`reject_unsupported_esp_framework` now takes
  the resolved build system and its message points at the switch), and
  `framework = arduino, espidf` still builds through the Arduino core with a
  warning — that pair is `integration=arduino-on-idf`, not component coverage.

### Delivery map

| Piece | PR | What landed |
|---|---|---|
| Tool components pinned | #1975 | one fbuild package per component, sha256 from the release's `tools/tools.json`; `linux-armel/armhf/i686` deliberately unpinned |
| Install profile | #1976 | esp32s3-only profile (`1149.2 MB` of toolchains) + the environment description |
| IDF source tree | #1978 | v5.5.1 checkout as a package (`450.0 MB`) |
| Python environment | #1980 | pinned constraints + lock, `85.5 MB` venv |
| Whole-release install | #1981 | `EspIdfInstall::install()` — one operation, no `install.sh` |
| CMake + Ninja | #1983 | Kitware CMake 3.30.2 + Ninja 1.12.1 as `Esp32S3Only` components (`153.6 MB`), addressed through `IdfComponent::root_rule()` because neither archive has a fixed container |
| Delegated build | #1984 | `board_build.build_system = espidf`, artifacts under `<build>/idf/`, supervised child with a structured failure |
| Proof gate | #1985 | `IdfCmakeProof` + `Esp32Integration::IdfCmake(proof)` |
| Compile database | #1986 | re-root + merge, published at the documented path |
| Caching decision | #1987 | `IdfCacheMode::ColdOnly` in the stack summary, the build log and the index |

The whole path is exercised end to end by
`crates/fbuild-build-esp/src/esp32/espidf_delegate_e2e_tests.rs` against the
committed fixture `tests/platform/espidf_component` (a real
`idf_component_register()` project with a second component and `app_main()`);
the ignored test passed in **48.4 s** against the pinned release.

## Spike — a real component project built under option 1

Acceptance criterion 1 of #1761 ("at least one spike that actually builds a
real component project and captures the artifacts") was run on 2026-11-09 on
the steward host (NixOS, 16 vCPU, load average ~60 during the build — a
deliberately *un*favourable host):

- ESP-IDF **v5.5.1** (`fcae3288`), shallow clone with submodules, 667 MB.
- Isolated toolchain install (`IDF_TOOLS_PATH`), `idf_tools.py install
  --targets=esp32s3`; gcc `xtensa-esp-elf 14.2.0 (esp-14.2.0_20241119)`.
- Project: root `CMakeLists.txt` with `include($ENV{IDF_PATH}/tools/cmake/project.cmake)`,
  a `main` component with `idf_component_register(SRCS "main.c" REQUIRES mycomp)`,
  a **second** component `components/mycomp` with its own
  `idf_component_register(SRCS ... INCLUDE_DIRS include)`, an `app_main()` with
  no `setup()`/`loop()`, and `sdkconfig.defaults`
  (`CONFIG_FREERTOS_HZ=1000`, 8 MB flash).
- `idf.py set-target esp32s3` then `idf.py build`.

Measured (all real, from that run):

| Phase | Wall time | Notes |
|---|---|---|
| toolchain install (all 3 targets) | 67 s | 3.8 GB, incl. 502 MB dist tarballs |
| `set-target esp32s3` (CMake configure) | 15.9 s | configure 12.0 s + generate 1.4 s |
| cold `build` | **50.3 s** | 1071 ninja actions: 1064 objects, 108 component archives |
| no-op `build` | 1.54 s | ninja graph up to date |
| `touch main/main.c` + `build` | 10.8 s | one object + link + image |

Artifacts captured (`build/`):

| Artifact | Size |
|---|---|
| `component_demo.bin` (app image) | 208,528 B (0x32e90) |
| `component_demo.elf` | 3,721,344 B |
| `component_demo.map` | 3,108,610 B |
| `bootloader/bootloader.bin` | 21,056 B (0x5240) |
| `partition_table/partition-table.bin` | 3,072 B |
| `sdkconfig` (generated from defaults) | 73,024 B |
| `compile_commands.json` | 7,391,003 B / 959 entries |
| total `build/` directory | 144 MB |

`idf.py size`: flash code 96,822 B + flash data 42,120 B, total image 208,417 B;
DIRAM 55,315 B / 16.19 %. Both registered components are present in the
component list (`main` and `mycomp`), i.e. the graph was resolved by IDF, not
by fbuild.

Portability findings (they are part of the cost, not incidental):
IDF's `install.sh` bootstrap **does not work unmodified on this host** —
the uv-provided CPython has no working `ensurepip`, so the IDF venv creation
fails; `openocd-esp32` fails its install check without a `libusb-1.0.so.0` on
the loader path. The spike needed a hand-built `uv venv`, a manually
downloaded `espidf.constraints.v5.5.txt`, and `LD_LIBRARY_PATH` for openocd.
Any delegating path must provision cmake/ninja/python/constraints itself
instead of assuming a working host interpreter.

Reference point from the org's own CI: FastLED's bare-IDF app
(FastLED/FastLED `ci/idf_app`, esp32, `espressif/idf:v5.5` image) is validated at
**240,368 B**, i.e. the same order as this spike's 208,528 B.

## Demand (research question 1)

The demand is proven *inside* the org, not speculated:

- `FastLED/FastLED` **is** an IDF component: root `CMakeLists.txt:79` calls
  `idf_component_register(SRCS ${FastLED_SRCS} INCLUDE_DIRS "src" REQUIRES ...)`.
- `FastLED/FastLED`'s `ci/idf_app/` is a pure ESP-IDF application (`app_main()`, no Arduino
  component) and `.github/workflows/build_bare_idf.yml` builds it for IDF 5.5
  (esp32, esp32s3, esp32c3, esp32c6, esp32p4) plus 5.1 and 4.4 (esp32,
  esp32s3). That is the reference implementation option 1 must match.
- pioarduino's platform tree ships `espidf-*` example projects built on
  `idf_component_register` (16 `espidf-*` examples in the cached 55.03.30 platform zip; 18 `CMakeLists.txt` there call `idf_component_register`), which any
  `platform = espressif32` + `framework = arduino, espidf` user can select.

Third-party demand is **not** measured here: GitHub code-search totals for
`idf_component_register` are too noisy to quote (the API caps and dedupes
results), so no ratio is claimed.

## Minimum viable inputs (research question 2)

What the spike actually consumed: project `CMakeLists.txt` + one per component;
the `idf_component_register()` graph (`SRCS`, `INCLUDE_DIRS`, `REQUIRES`);
`sdkconfig.defaults` → generated `sdkconfig` (73 KB) including Kconfig
defaults; IDF linker scripts (`esp32s3.rom.newlib.ld`,
`esp32s3.peripherals.ld`); the partition table; and a **separate bootloader
project** (the bootloader is a second ELF, not a by-product of the app link).
`EXTRA_COMPONENT_DIRS` and `idf_component.yml` (component manager) are
additional inputs whenever a project uses managed components.

## Delegation, caching and the compile database (research question 3)

- `compile_commands.json` is produced by CMake: 959 entries, absolute source
  and flag paths. fbuild already rewrites a compile database for clangd
  (`crates/fbuild-build-engine/src/compile_database/database.rs:11`), so
  re-rooting a delegated database is the same problem with the same tooling —
  no new design needed.
- Caching: IDF drives **gcc directly from ninja**, so there is no CMake-level
  cache. The only insertion point for zccache is a compiler wrapper
  (`CC`/`CXX`/`CMAKE_C_COMPILER_LAUNCHER`) exported by `idf_tools`; without one,
  delegation inherits ninja's incremental behaviour (10.8 s for one file, 50.3 s
  cold here) and has no zccache. The wrapper was measured and **rejected** —
  see "Caching decision" below.
- Artifacts land in `<project>/build/` (144 MB per project/config) and are the
  proof of what was built: ELF, bin, map, sdkconfig, partition table,
  compile database.

## Caching decision (follow-up 5, FastLED/fbuild#1974)

Measured 2026-11-10 on the steward host (16 vCPU, load average 9-30 during the
runs, so the deltas below are far outside that noise) against the pinned release
(v5.5.1, `fcae3288`) and the spike project now committed as
`tests/platform/espidf_component`: 1047 ninja compile actions, 8 MB flash.
Harness: `fbuild-steward/probes/idf_cache_bench.py`, which reconstructs the
delegated child environment (`IDF_PATH`, `IDF_PYTHON_ENV_PATH`,
`IDF_TOOLS_PATH`, the profile's `PATH` entries prepended to the host `PATH`) and
passes the launcher as `-DCMAKE_C_COMPILER_LAUNCHER=… -DCMAKE_CXX_COMPILER_LAUNCHER=…`
— the only insertion point that exists, because ninja, not CMake, spawns gcc.

| Run | Wall time | Cache behaviour |
|---|---|---|
| cold, no launcher | **41.8 s** | configure 6.2 s + 1047 compiles |
| no-op (`ninja` up to date) | **1.2 s** | the existing build dir |
| `idf.py fullclean` + build, no launcher | **34.0 s** | same build dir |
| cold, `zccache` launcher, empty cache | 50.6 s | 233 MB stored |
| new build dir, launcher, cache warm | 67.4 s | **no reuse** — a second full object set (436 MB) |
| `idf.py fullclean` + build, launcher | 110.6 s | **no reuse** — a third set (640 MB) |

`zccache --show-stats` after those runs: 2838 compilations, 0.2 % hit rate. The
wrapper is not broken — replaying one identical entry (same cwd, same argv) does
hit — but the key it computes covers ninja's command line, which embeds the
absolute build directory (`-o`, `-MF`, `-I`, `-fmacro-prefix-map`). Every build
directory, and every `fullclean`, therefore re-compiles everything, at 1.2-3.3x
the unwrapped wall time. The only reuse the wrapper could offer inside one build
directory is a rebuild that ninja already serves as a 1.2 s no-op.

**Decision: cold-only, declared.** No compiler wrapper is wired. The delegated
build reports the mode rather than leaving a slow build unexplained:

- the stack summary carries `cache=cold-only` next to `integration=idf-cmake`;
- the build log gets an `idf.py caching: cold-only …` line naming ninja as the
  compiler driver;
- `<build>/idf/idf-artifacts.json` records `"cache": "cold-only"`.

A future launcher has to change that value (and the tests that pin it), and its
cache key has to cover the IDF release and the `sdkconfig` digest instead of the
raw command line — the measurement above is the cost of entry, not a design
sketch. Storing 640 MB of duplicates for zero hits is not a trade this path
makes.

## Metadata / false-coverage guard (research question 4) — delivered in #1972/#1985

`esp32_package_stack_summary` records the request *and* the effective
integration (tested in
`orchestrator/tests.rs::stack_summary_records_requested_framework_and_integration_mode`):

```
framework=arduino, espidf framework_pin=... resolved platform=espressif32@6.13.0, framework=framework-arduinoespressif32@3.3.7 integration=arduino-on-idf
```

`integration=idf-cmake` is a claim about what a build *did*, and it is now
unrepresentable without proof. `IdfCmakeProof` (`orchestrator/helpers.rs`) has
private fields and a single constructor, `verified(artifacts, idf_release)`,
which measures a non-empty app image and ELF, at least one
`compile_commands.json` entry and the SHA-256 of the generated `sdkconfig`; a
build that cannot produce that proof is an **error**, never a silent downgrade
to a weaker label. `Esp32Integration::IdfCmake(proof)` carries it, and
`RequestedFrameworks::integration` can never produce it, so a request cannot
manufacture the claim. Only `esp32_idf_cmake_stack_summary` may spell it:

```
ESP32 packages: build_system=espidf, chip=esp32s3, integration=idf-cmake, cache=cold-only, idf=v5.5.1; proof: app_bin_bytes=..., elf_bytes=..., compile_db_entries=..., sdkconfig_sha256=...
```

A bare `espidf` request without the opt-in still fails fast, so the two
protections compose: the request gate (#1730) and the artifact gate (#1972).

## Toolchain cost (research question 5)

Measured on the esp32s3-only install (same host, `du -sh`):

| Component | Size |
|---|---|
| `xtensa-esp-elf` toolchain (needed) | 1.1 GB |
| `riscv32-esp-elf` toolchain (installed by default, unneeded for S3) | 2.1 GB |
| `xtensa-esp-elf-gdb` (debug only) | 105 MB |
| python env | 63 MB |
| `openocd-esp32` | 11 MB |
| `esp32ulp-elf` + `esp-rom-elfs` | 38 MB |
| downloaded dist tarballs (deletable) | 502 MB |
| IDF source checkout, **per IDF version** | 667 MB |

So the honest marginal cost of one extra IDF version for one chip family is
~1.2 GB of toolchain + 667 MB of source, versus the current Arduino-on-IDF
stack already installed here (`framework-arduinoespressif32-libs` 1.9 GB +
`toolchain-xtensa-esp-elf` 1.2 GB).

**As delivered** (measured on the esp32s3 profile, `EspIdfInstall`): toolchains
**1149.2 MB** (the #1970 AC2 budget is 1.3 GB), IDF source tree **450.0 MB**,
Python env **85.5 MB** (312.6 MB including its CPython), CMake + Ninja
**153.6 MB** — about **1.8 GB** per release for one chip family, installed as a
single fbuild operation with every archive sha256-pinned from the release's own
`tools/tools.json` (no `install.sh`, no host `~/.espressif`). Two environment
requirements were measured and are pinned by tests: `IDF_TOOLS_PATH` must point
at the python entry's `python_env/` (constraints lookup is offline, so a missing
file is fatal), and the profile's `PATH` entries are *prepended* to the host
`PATH` rather than substituted, because `idf.py`/CMake shell out to `ln`/`git`.

Per-chip slices (#1949/#1951) cut the *framework* half; an IDF release has no
equivalent slicing story — the profile is chip-scoped (esp32s3), but a second
family adds its own cross toolchain.

## Options

1. **Delegate to `idf.py`/CMake.** Fastest route to correctness, and the
   artifact set above is exactly the proof we need. Cost: +1.2 GB/version/chip
   family, a second build system to supervise, no zccache unless the compiler
   is wrapped, and cmake/ninja/python must be provisioned by fbuild (the host
   bootstrap failure above is the warning).
2. **Native IDF component model in Rust.** Re-implements Kconfig, linker
   fragments, bootloader and partition generation, and would lag every IDF
   release; measured surface (1064 objects and 108 component archives) makes this a
   permanent treadmill. Not recommended.
3. **Hybrid (chosen and delivered).** Native Arduino-on-IDF stays the default and keeps
   zccache, slices and its current diagnostics. Genuine component projects opt
   in to delegation, which must publish its artifacts as proof.

## Follow-ups (all delivered)

1. Toolchain: pin an ESP-IDF release (source + toolchains) as fbuild packages,
   with an esp32s3-only install profile so the default is ~1.2 GB, not 3.8 GB.
   **Done in #1975/#1976/#1978/#1980/#1981/#1983** — the delivered profile
   installs ~1.8 GB for esp32s3, including the source tree, the Python env and
   CMake/Ninja (the "Toolchain cost" table has the split).
2. `fbuild build` delegating path behind the explicit opt-in, producing
   `<build>/idf/` artifacts (elf, bin, map, sdkconfig, compile DB).
   **Done in #1971 / PR #1984** — `espidf_delegate.rs` + `espidf_artifacts.rs`;
   the artifact set is discovered from IDF's own `flasher_args.json` and
   indexed in `idf-artifacts.json`.
3. False-coverage gate: `integration=idf-cmake` only with artifact proof
   (bytes + compile-db entries + sdkconfig digest); extend the #1730 summary
   test. **Done in #1972 / PR #1985** — `IdfCmakeProof` (private fields, one
   `verified` constructor); `delegate_build` fails rather than downgrading.
4. Compile-database re-rooting + daemon/indexer merge for delegated builds.
   **Done in #1973** (`compile_database::merge` + `espidf_compile_db`): entries
   are keyed by the normalized absolute source path, fbuild's entry wins an
   overlap, the delegated invocation (`command` or `arguments`) is carried over
   unsplit, the file is written at `CompileDatabase::expected_output_path`
   (the native path's location) and is byte-identical between two identical
   builds. `directory` becomes the project directory and `file`/`output` are
   spelled relative to it, so no scratch build-directory or IDF-checkout prefix
   survives (`..` is kept for framework sources; a Windows drive split falls
   back to the absolute spelling, which is the only resolvable option).
5. Caching decision: compiler wrapper into zccache, or document a cold-only
   path with its own metric. **Decided cold-only** and implemented (#1974) — the
   wrapper measured slower with no reuse, see below.
6. Report `framework = arduino, espidf` honestly everywhere (it is
   `integration=arduino-on-idf`) so no board is described as component
   coverage before follow-up 2 lands. **Done** — the stack summary states the
   integration mode, the cache mode and the IDF release, and
   `integration=idf-cmake` is only reachable through proof.

## What remains

- **The switch is per environment.** No project gets a delegated build by
  accident; a project has to set `board_build.build_system = espidf`.
- **The native IDF-only path (#1878) is a separate, still-draft track** for the
  silicon families pioarduino cannot serve (C5, P4). #1880 (sdkconfig in the
  sketch root + an in-sketch directive) is scoped to it and gated on it.
- **No per-chip slicing of a release**, and no second chip family's profile
  measured (see "Toolchain cost").
- **No CI lane builds the delegated fixture**: the end-to-end test is `#[ignore]`
  and run deliberately (48.4 s here); wiring a hosted lane was an #1974
  non-goal.
- **A compiler-launcher cache** stays a future option with a cost of entry: its
  key must cover the IDF release and the `sdkconfig` digest rather than the raw
  compile command (see "Caching decision").

## Non-goals (held through delivery)

- No CMake or `idf_component_register()` coverage claim until artifacts prove
  the registration executed and the intended entry point ran.
- No regression of the current Arduino path and no weakening of #1730's
  rejection of unsupported bare `espidf`.
- **Held:** no coverage claim is made anywhere that artifacts do not back, the
  Arduino path is the same code path it was, and a bare `espidf` request
  without the opt-in still fails fast.
