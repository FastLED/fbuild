# fbuild-build

Build orchestration, compilation, linking for all platforms (AVR, ESP32, RP2040, STM32, Teensy, WASM).

## Modules

- **avr/** — AVR-GCC compiler and build orchestrator (Arduino Uno, Mega, etc.)
- **esp32/** — ESP32 RISC-V/Xtensa compiler and orchestrator (esp32, esp32c6, esp32s3, esp32p4)
- **teensy/** — ARM Cortex-M7 compiler and orchestrator (Teensy 4.x)
- **compile_database** — `compile_commands.json` generation for clangd/VS Code IntelliSense
- **compiler** — `Compiler` trait and `CompilerBase` shared utilities
- **linker** — `Linker` trait for platform-specific linking
- **parallel** — Parallel compilation with job control
- **source_scanner** — Source file discovery (sketch, core, variant)

## Native `extra_scripts` Boundary

fbuild evaluates PlatformIO `extra_scripts` in a Python subprocess sidecar (`lite_scons_harness.py`). It's the only backend as of [#553](https://github.com/FastLED/fbuild/issues/553) step 4 (the legacy MockEnv shim retired); see that issue for the staged migration history.

### What's supported

- `pre:` and `post:` script entries
- `Import("env")` / `Import("projenv")` (both work in PRE and POST scripts; the MockEnv-era PRE-rejects-projenv rule no longer applies)
- `from SCons.Script import DefaultEnvironment` followed by `DefaultEnvironment()`
- `Append`, `AppendUnique`, `Prepend`, and `Replace` over `CPPDEFINES`, `CPPPATH`, `CCFLAGS`, `CFLAGS`, `CXXFLAGS`, `ASFLAGS`, `LINKFLAGS`, `LIBPATH`, `LIBS`, and `BUILD_FLAGS` (which folds into the common compile flags on export)
- tuple-shaped `CPPDEFINES` appended in place (`env["CPPDEFINES"].append(("NAME", value))`) export as `-DNAME=value`
- project introspection: `GetBuildType`, `GetProjectOptions`, `GetProjectOption`, `GetProjectConfig`, `env.get(key, default)` (falls through `_vars` → `_scopes` → `project_options` → default)
- helper shims: `Dump`, `BoardConfig`, `PioPlatform`, `Flatten`, `VerboseAction`, `IsCleanTarget`, `IsIntegrationDump`
- effectful `env.Execute(env.Action(callable_or_cmd))` — the callable runs in the same subprocess (PlatformIO's working directory), command-strings run via shell with `env.subst` applied first. Generated files materialise on disk and surface in the `lite_scons_records.generated_files` manifest fbuild reads on the Rust side.
- `env.AddPreAction(target, action)` / `env.AddPostAction(target, action)` — recorded with the **unresolved** target template (e.g. `$BUILD_DIR/$PROGNAME$PROGSUFFIX`) so fbuild can `subst` it at deploy time when the values are known.
- `env.AddBuildMiddleware(callback, regex=None)` — recorded with the callback name and glob; fbuild's native compile pipeline can invoke it per matching source.
- `env.AddCustomTarget(name, dependencies=None, actions=None, ...)` — recorded as a custom target.
- `env.SConscript("child.py", exports=None)` — recursively execs the child against the same env. Paths resolve relative to the calling script's directory (matching real SCons), not the project root.
- `env.AddMethod(callable, name=None)` — installs as `env.<name>(...)` for script-defined helpers.
- `env.ParseFlagsExtended("-Ipath -I sep/inc -DK=V -lname -Wl,…")` — routes tokens into the right scopes; handles both joined (`-Ipath`) and space-separated (`-I path`) forms for `-I`/`-L`/`-l`.
- non-flag tool/output scopes (e.g. `MKSPIFFSTOOL`, `PROGNAME`, `UPLOAD_PROTOCOL`) — `Replace`/`Append` mutations are stored on the env; tool-path scripts don't abort the native build.

### Architectural boundaries

Three categories deliberately fall through to `--platformio`:

1. **Real DAG / incremental rebuilds.** The harness does a single-pass resolve-then-return. Generated sources must be regenerated each clean build.
2. **Scanner-driven header dep discovery.** fbuild has its own `fbuild-header-scan`; the harness doesn't replicate SCons `CScanner`.
3. **PlatformIO-defined chip-family builders** (`env.MergeFlashImage` for ESP32, `env.PackageJsonFirmware` for OTA, etc.) — the harness records these as `builder_calls` entries; fbuild maps known names to native `fbuild-deploy` ops, otherwise fails fast with a targeted "needs `--platformio` for builder X" message.

### `build_flags = !python ...` stdout-capture pattern

Out of scope for both backends — only `extra_scripts` entries are interpreted.

## Compile Database (compile_commands.json)

After every build, fbuild generates a [JSON Compilation Database](https://clang.llvm.org/docs/JSONCompilationDatabase.html) so that clangd and VS Code IntelliSense can resolve includes to actual source files.

- Written to both the build directory and the project root (for clangd auto-discovery)
- Uses individual `-I` flags (never `@file` response file references)
- `file` field points to the actual source path, not a build-directory copy
- Cache wrappers (sccache/zccache/ccache) are stripped from compiler paths
- **Library projects** (detected via `library.json` at project root) suppress the project-root copy to avoid overwriting meson/cmake-generated files

## zccache backend (`embedded` Cargo feature)

Two backends route compiles through zccache:

| Backend | Built when | Selected when | What runs per compile |
|---|---|---|---|
| `CompileBackend::Embedded` | `embedded` feature (default) | default | in-process `ZccacheService` inside `fbuild-daemon`'s tokio runtime |
| `CompileBackend::Wrapped` | always | `FBUILD_ZCCACHE_EMBEDDED=0` opt-out, or `--no-default-features` build | `zccache wrap <compiler> <args>` child process from the managed binary |

**As of Phase 4 ([FastLED/fbuild#793](https://github.com/FastLED/fbuild/issues/793))**
the embedded backend is the default. Every compile dispatches
through the in-process `ZccacheService::compile` running on the
daemon's tokio runtime — no per-TU `zccache.exe` child process is
spawned. Fingerprint operations (`check`, `mark_success`) likewise
route through the embedded `TwoLayerCache` instead of `zccache fp`
shellouts ([Phase 3 / #792](https://github.com/FastLED/fbuild/issues/792)).

### Opting out

Set `FBUILD_ZCCACHE_EMBEDDED=0` at daemon startup to force the
legacy wrapper path. This is the recommended way to debug regressions
attributable to the embedded service — it pins the wrapper-mode
code path for the running daemon without changing the binary.

Building with `--no-default-features` drops the `embedded` feature
entirely, so the daemon ships without the zccache library and falls
back to wrapper mode unconditionally. Use this for size-constrained
CI lanes that can't pay the zccache library's compile-time cost.

### Embedded service fallback

If the embedded service fails to start (e.g. permissions on the
cache root, disk full), the daemon logs a warning and falls back
to wrapper mode for that run — no build is ever *prevented* by an
embedded-service failure. Same fallback applies if an individual
per-compile dispatch fails; the wrapper-mode arms remain in the
source tree as the safety net.

The wrapper-mode code (`managed_zccache.rs`,
`zccache::ensure_running`, `zccache::wrap_args`, the `compile_source`
wrapper arms) will be retired in a follow-up after one release cycle
of validation in the embedded default (Phase 4 stage 2).
