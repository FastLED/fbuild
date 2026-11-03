# ESP-IDF-only builds (round 1)

## Public surface

```ini
[env:esp32s3-idf]
platform = espressif32
board = esp32-s3-devkitc-1
framework = espidf
```

Write `void app_main(void)` in `src/main.c`, then run:

```sh
fbuild build . -e esp32s3-idf
fbuild deploy . -e esp32s3-idf
```

This is an IDF 5.4 CMake/ninja component build, with no Arduino core. This
round enables **ESP32-S3 only**, using a coherent pinned package set. Arduino
and `arduino, espidf` continue through the existing Arduino pipeline, including
the warning that the latter is not a component build. Other bare-IDF targets
fail explicitly; their board metadata is not evidence of implemented support.
No CLI option, workspace crate, Rust dependency or workflow is added.

## Ownership and project layout

fbuild resolves the framework and matching compiler through its existing
PlatformIO registry client, fleet mirror, checksum verification and staged
package cache. The pins in `orchestrator/idf/packages.json` come from PlatformIO
espressif32 v6.10.0: `framework-espidf@3.50400.0` (IDF 5.4), and
`toolchain-xtensa-esp-elf@14.2.0+20241119`. Both are already in the mirror.
There is no second mirror or git clone of IDF at build time. The existing
`Esp32Toolchain` locates the compiler and size binary.

IDF owns Kconfig, component dependencies, linker scripts, compilation, linking,
partition generation, ELF-to-image conversion and image-size checks. fbuild
invokes CMake with the Ninja generator, then `cmake --build`; it does not
reconstruct that graph in Rust. IDF's own CMake rules invoke esptool for image
conversion. Hardware flashing still goes through `fbuild deploy`.

A project-owned root `CMakeLists.txt` is used directly. IDF discovers `main/`,
`components/`, `EXTRA_COMPONENT_DIRS` and component-manager manifests by its
normal rules. For a minimal `src/` project without a root CMake file, fbuild
writes a project under the environment build directory: it adds the project's
`components/`, uses `src/CMakeLists.txt` as an extra component if present, or
wraps C/C++/assembly sources in an IDF `main` component. It never writes CMake
files into the source tree. Native projects should register `src/` themselves
or use IDF's usual `main/` layout.

CMake/ninja and upstream Python requirements run in a per-environment venv,
installed through uv and its shared download cache,
with Python 3.11 and versioned CMake/ninja packages. IDF's upstream 5.4 constraints
file constrains its requirements. This needs uv, network on first use, and a
supported native host for the mirrored compiler. It does not install Python
application logic in fbuild. `idf.py` is unnecessary for the build: it is itself
an orchestrator over the same CMake/ninja rules.

## Configuration

`docs/sdkconfig.md` describes an Arduino-oriented **proposal**, not an existing
four-layer general-purpose resolver. The bare-IDF baseline is IDF's Kconfig
configuration, not Arduino's precompiled `sdkconfig.h`.

The adapter seeds an environment-local `idf/sdkconfig` with, in increasing
precedence:

1. `sdkconfig.defaults` (or files in `board_build.sdkconfig_defaults`), then
   `sdkconfig.defaults.esp32s3`;
2. the project `sdkconfig`, or `board_build.esp-idf.sdkconfig_path`;
3. `sdkconfig.fragment`;
4. `board_build.esp-idf.sdkconfig_options`;
5. `build_flags` / caller `-DCONFIG_*` values.

IDF parses the Kconfig text and produces `config/sdkconfig.h`. CONFIG flags are
consumed as Kconfig input instead of also redefining generated header macros.
Use Kconfig values (`y`, `n`, quoted strings) for these options. On input change,
fbuild reseeds the generated configuration so a previous build cannot mask an
overlay change. Unchanged inputs preserve IDF's resolved configuration. No
project `sdkconfig` is modified. The future `fbuild.toml` layer is not added.

Ordinary `build_flags` pass to CMake C/C++ compiler flags. Use component CMake
for finer control. `board_build.cmake_extra_args` accepts shell-quoted `-D...`
arguments, for example `-DEXTRA_COMPONENT_DIRS=/absolute/components`; native IDF
variables remain the extension mechanism. Target, sdkconfig, Python and toolchain
selection are reserved to the validated adapter. `board_build.partitions` accepts
a project-relative CSV; native Kconfig partition options also work, subject to
the flash layout restriction below. There is no new wrapper around individual
IDF component or CMake features.

`fbuild install` currently rejects bare-IDF environments with guidance to use
`fbuild build`, which provisions the correct packages automatically.

This initial set rejects platform version/URL/package overrides, extra scripts,
source filtering/flags, build_unflags, custom_sdkconfig, compiledb-only,
symbol-analysis, bloat and shrink modes rather than silently treating them as
implemented. Extend these contracts with focused tests as needed.

## Artifacts and scope

IDF output stays under `<build_dir>/idf/`, including `build.ninja`, sdkconfig,
`project_description.json`, `flasher_args.json` and the native component objects.
fbuild exports `firmware.elf`, `firmware.bin`, `bootloader.bin`, `partitions.bin`
and `compile_commands.json` at the existing build root and uses its existing
size tool/parser and build log report. IDF still enforces the application
partition's image limit; fbuild's displayed maximum is the board's memory limit.

The current deployer assumes MCU-defined offsets. The adapter verifies IDF's
flash manifest against those offsets and rejects layouts requiring extra images
(e.g. OTA initialization data), relocated partition tables or relocated apps.
The accepted default S3 layout uses bootloader 0x0, partitions 0x8000 and app
0x10000. Flash-mode/frequency Kconfig settings should agree with the board's
upload settings. Hardware deployment is not claimed as tested without a board.

## PlatformIO implementation research

Sources are pinned to [platform-espressif32 v6.10.0, commit
4d72e44](https://github.com/platformio/platform-espressif32/tree/4d72e44ebe60c2fde58496f9a72a0e0dd4861cf7),
not a blog or a claim inferred from framework names:

- [platform.json](https://github.com/platformio/platform-espressif32/blob/4d72e44ebe60c2fde58496f9a72a0e0dd4861cf7/platform.json)
  declares framework/tool packages. The pure-IDF Xtensa path selects the unified
  compiler package; older Arduino uses per-chip xtensa packages, while RISC-V
  boards select riscv32-esp tools. Package/host/version selection precedes builds.
- [builder/frameworks/espidf.py](https://github.com/platformio/platform-espressif32/blob/4d72e44ebe60c2fde58496f9a72a0e0dd4861cf7/builder/frameworks/espidf.py)
  `create_default_project_files`, `get_cmake_code_model`, `run_cmake`,
  `populate_idf_env_vars`, `build_components`, `build_bootloader` and
  `generate_project_ld_script` show the actual boundary.
- [builder/main.py](https://github.com/platformio/platform-espressif32/blob/4d72e44ebe60c2fde58496f9a72a0e0dd4861cf7/builder/main.py)
  defines compiler tools, image conversion, partition-size accounting and upload
  commands. [ESP-IDF's project.cmake](https://github.com/espressif/esp-idf/blob/v5.4/tools/cmake/project.cmake)
  defines the underlying native project contract.

The downloaded `framework-espidf` package is the source SDK, not Arduino's
precompiled libraries: it contains `components/`, `tools/cmake/`,
`tools/idf.py`, `tools/requirements/requirements.core.txt`, Kconfig definitions,
examples and `package.json` (`3.50400.0`). The component and tools trees include
the packaged submodule contents needed by the build; fetching a GitHub source
archive without those contents would not be equivalent.

PlatformIO generates missing root and src CMake files **in the project**. Its
root includes IDF `project.cmake`; its src file registers recursively collected
sources. It passes a non-main `src/` through `EXTRA_COMPONENT_DIRS`; it also adds
a dummy framework component to extract generic compile flags. IDF discovers
component paths/dependencies, and PlatformIO reads `project_description.json`
and CMake's codemodel-v2 replies to recover targets, sources, includes, defines,
compile groups and link fragments.

PlatformIO passes `SDKCONFIG` (by default project `sdkconfig.<environment>`,
overridable with `board_build.esp-idf.sdkconfig_path`) to CMake. IDF's Kconfig
machinery loads that active file and defaults (`sdkconfig.defaults`, target
variants, or native `SDKCONFIG_DEFAULTS`) and emits generated headers/config JSON.
`board_build.cmake_extra_args` is passed into CMake. **The inspected builder does
not implement `board_build.esp-idf.sdkconfig_options`**; that spelling in fbuild's
proposal must not be mistaken for verified PlatformIO merging behavior.
PlatformIO combines CMake-extracted flags with SCons/project `build_flags`;
CONFIG compiler defines are not a substitute for rebuilding Kconfig components.
`board_build.partitions` chooses a CSV before PlatformIO runs IDF's partition
script and updates upload size/offsets.

PlatformIO delegates configuration and generated configuration/header information
to CMake/IDF, but **reimplements much of the application and bootloader build in
SCons**: `build_components` compiles CMake-selected sources into SCons archives,
extracts link arguments, generates linker scripts with IDF tools, links the ELF,
runs IDF's partition generator, and uses Python/esptool `elf2image` and
`write_flash` commands. It does not simply run `idf.py build` or hand the whole
application to ninja. Ninja build information is inspected for linker fragments;
CMake uses the Ninja generator, but that does not mean ninja owns the complete
PlatformIO application build. Python environments, component discovery adaptation,
SCons library dependency discovery and special generated assets are also handled
by the platform builder.

fbuild deliberately does not copy the codemodel-to-SCons translation, dummy SDK
component, framework-tree mutation, source-tree CMake generation or parallel
linker-script/partition implementation. Running the native build preserves IDF's
component contract with a smaller integration surface.

## Validation

The fixture is `tests/platform/esp32s3-idf`. Run the real opt-in Rust test in CI:

```sh
soldr cargo test -p fbuild-build-esp --lib espidf_s3_real_build -- --ignored --nocapture
```

The test crosses the real registry/download/compiler/CMake boundary and asserts
flash artifacts, IDF build metadata, generated Kconfig and fbuild's size result.
Ordinary unit tests cover dispatch, target/pin rejection, config input changes
and reserved CMake knobs. This is an explicit opt-in test; routine CI does not
run it merely because the fixture exists.

## Next targets

ESP32 and S2 need Xtensa target/tool prefix validation with this SDK. C2, C3, C6
and H2 need a matching RISC-V package and real builds; S3 ULP applications also
need their coprocessor toolchain. C5 and P4 need explicit supported SDK/version
selection and matching RISC-V packages (including P4 revision distinctions), not
just enabling a name in a list. The same native IDF adapter can serve these once
the coherent package/target matrix has evidence. Dual Arduino-as-an-IDF-component
builds are a separate feature; current dual mode remains Arduino-backed.
