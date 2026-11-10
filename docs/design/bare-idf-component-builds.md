# Bare ESP-IDF / CMake component builds — research and recommendation

**Status:** research complete (spike below), no implementation · **Issue:** FastLED/fbuild#1761
(parent #1664; the *reject* half landed in #1730) · **Date:** 2026-11-09

fbuild builds ESP32 projects only through the Arduino core and its precompiled
ESP-IDF libraries. A bare `framework = espidf` project is now rejected instead
of silently producing an Arduino-only build (#1730), but there is still no way
to *honor* a genuine CMake/`idf_component_register()` project. This note
compares the options with measured evidence and recommends one.

## Recommendation

**Option 3 (hybrid), opt-in, proof-gated.** Keep the native Arduino-on-IDF path
exactly as it is, and add a delegating path that runs the project's own
`idf.py`/CMake build against a pinned ESP-IDF release, then records the
artifacts it produced. Do not implement the native component model (option 2):
the measured cost of delegation is bounded and the cost of re-implementing
IDF's build layer is unbounded.

The delegation must be **explicit** (a project opt-in, e.g.
`board_build.build_system = espidf`), because it changes the caching story,
the install footprint (+1.2 GB per IDF version per chip family, measured below)
and the meaning of "cold build".

## What exists today

- The ESP32 path resolves pioarduino (`platform-espressif32` +
  `framework-arduinoespressif32`) and compiles sketch, cores and libraries
  itself — `crates/fbuild-build-esp/src/esp32/orchestrator/packages.rs`.
- IDF is consumed as **precompiled `.a` libraries** linked by fbuild's own
  recipe — `crates/fbuild-build-esp/src/esp32/esp32_linker.rs:3-4`
  ("100+ precompiled `.a` libraries from ESP-IDF").
- There is **no CMake and no `idf.py`** anywhere in the ESP crates:
  `crates/fbuild-build-esp/src/esp32/orchestrator/helpers.rs:18,137` state it
  verbatim, and `reject_unsupported_esp_framework` (`helpers.rs:143-168`)
  errors for `framework = espidf` alone while only warning for
  `arduino, espidf`.
- `framework = arduino, espidf` therefore links IDF *through the Arduino core*.
  FastLED's `ci/boards.py:900` (`ESP32DEV_IDF5_COMPONENT`) declares exactly
  that framework pair, so the claim that it exercises
  `idf_component_register()` is not proven under fbuild.

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
(`ci/idf_app`, esp32, `espressif/idf:v5.5` image) is validated at
**240,368 B**, i.e. the same order as this spike's 208,528 B.

## Demand (research question 1)

The demand is proven *inside* the org, not speculated:

- `FastLED/FastLED` **is** an IDF component: root `CMakeLists.txt:79` calls
  `idf_component_register(SRCS ${FastLED_SRCS} INCLUDE_DIRS "src" REQUIRES ...)`.
- `ci/idf_app/` is a pure ESP-IDF application (`app_main()`, no Arduino
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
  (`CC`/`CXX`/`CMAKE_C_COMPILER_LAUNCHER`) exported by `idf_tools`; unless
  fbuild wraps the compiler, delegation inherits ninja's incremental behaviour
  (10.8 s for one file, 50.3 s cold here) and loses zccache entirely.
- Artifacts land in `<project>/build/` (144 MB per project/config) and are the
  proof of what was built: ELF, bin, map, sdkconfig, partition table,
  compile database.

## Metadata / false-coverage guard (research question 4)

`esp32_package_stack_summary` (`helpers.rs`, tested in
`orchestrator/tests.rs::stack_summary_records_requested_framework_and_integration_mode`)
already records the request *and* the effective integration:

```
framework=arduino, espidf framework_pin=... resolved platform=espressif32@6.13.0, framework=framework-arduinoespressif32@3.3.7 integration=arduino-on-idf
```

That is **sufficient to prevent false claims today**, because a delegated
build would need a new value (`integration=idf-cmake`) that only the delegating
path can emit. The guard to add with it: `integration=idf-cmake` is only valid
when the build result carries proof — ELF and app bin bytes, the entry count of
`compile_commands.json`, and the `sdkconfig` digest. A bare `espidf` request
continues to fail fast (`helpers.rs:143-168`).

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
`toolchain-xtensa-esp-elf` 1.2 GB). Per-chip slices (#1949/#1951) already cut
the framework half; an IDF release has no equivalent slicing story yet.

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
3. **Hybrid (recommended).** Native Arduino-on-IDF stays the default and keeps
   zccache, slices and its current diagnostics. Genuine component projects opt
   in to delegation, which must publish its artifacts as proof.

## Proposed follow-ups

1. Toolchain: pin an ESP-IDF release (source + toolchains) as fbuild packages,
   with an esp32s3-only install profile so the default is ~1.2 GB, not 3.8 GB.
2. `fbuild build` delegating path behind the explicit opt-in, producing
   `<build>/idf/` artifacts (elf, bin, map, sdkconfig, compile DB).
3. False-coverage gate: `integration=idf-cmake` only with artifact proof
   (bytes + compile-db entries + sdkconfig digest); extend the #1730 summary
   test.
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
   path with its own metric.
6. Report `framework = arduino, espidf` honestly everywhere (it is
   `integration=arduino-on-idf`) so no board is described as component
   coverage before follow-up 2 lands.

## Non-goals

- No CMake or `idf_component_register()` coverage claim until artifacts prove
  the registration executed and the intended entry point ran.
- No regression of the current Arduino path and no weakening of #1730's
  rejection of unsupported bare `espidf`.
- No implementation in this pass.
