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

fbuild evaluates PlatformIO `extra_scripts` in a Python subprocess sidecar. As of [#553](https://github.com/FastLED/fbuild/issues/553) step 3, two backends coexist:

| Backend | When it runs | What it covers |
|---|---|---|
| **lite-SCons** (`lite_scons_harness.py`) — **default** | `FBUILD_LITE_SCONS` unset, or any value other than `0`/`false`/`no`/`off` | Everything MockEnv covered + effectful `env.Execute(...)`, `env.AddPreAction`/`AddPostAction`, `env.AddBuildMiddleware`, `env.AddCustomTarget`, recursive `env.SConscript`, `env.AddMethod`, `env.ParseFlagsExtended` (both `-Ipath` and `-I path`) |
| **MockEnv** (`script_runtime_harness.py`) — legacy | `FBUILD_LITE_SCONS=0` / `false` / `no` / `off` | The pre-#553 surface: flag/path mutations only, with hard-fail on `Execute` / `SConscript` / middleware |

The lite backend is a functional superset of MockEnv: every script that worked under MockEnv continues to work, and four additional clusters of scripts (generator scripts, Marlin-class middleware, OTA `merge_bin` post-actions, recursive `SConscript` chains) now native-build instead of falling back to `--platformio`.

MockEnv is on the retirement track defined in the [#553 plan](https://github.com/FastLED/fbuild/issues/553); step 4 will delete it. Until then `FBUILD_LITE_SCONS=0` is the emergency opt-out if the lite backend ever produces a wrong overlay for a real-world project.

### Lite-SCons-only primitives

These are what the lite backend adds on top of MockEnv:

- `env.Execute(env.Action(callable_or_cmd))` — the callable runs in the same subprocess (PlatformIO's working directory), command-strings run via shell with `env.subst` applied first. Generated files materialise on disk and surface in the `lite_scons_records.generated_files` manifest fbuild reads on the Rust side.
- `env.AddPreAction(target, action)` / `env.AddPostAction(target, action)` — recorded with the **unresolved** target template (e.g. `$BUILD_DIR/$PROGNAME$PROGSUFFIX`) so fbuild can `subst` it at deploy time when the values are known.
- `env.AddBuildMiddleware(callback, regex=None)` — recorded with the callback name and glob; fbuild's native compile pipeline can invoke it per matching source.
- `env.AddCustomTarget(name, dependencies=None, actions=None, ...)` — recorded as a custom target.
- `env.SConscript("child.py", exports=None)` — recursively execs the child against the same env. Paths resolve relative to the calling script's directory (matching real SCons), not the project root.
- `env.AddMethod(callable, name=None)` — installs as `env.<name>(...)` for script-defined helpers.
- `env.ParseFlagsExtended("-Ipath -I sep/inc -DK=V -lname -Wl,…")` — routes tokens into the right scopes; handles both joined (`-Ipath`) and space-separated (`-I path`) forms for `-I`/`-L`/`-l`.

### Always-supported (both backends)

- `pre:` and `post:` script entries
- `Import("env")` in PRE/POST scripts; `Import("projenv")` in POST scripts only
- `from SCons.Script import DefaultEnvironment` followed by `DefaultEnvironment()`
- `Append`, `AppendUnique`, `Prepend`, and `Replace` over `CPPDEFINES`, `CPPPATH`, `CCFLAGS`, `CFLAGS`, `CXXFLAGS`, `ASFLAGS`, `LINKFLAGS`, `LIBPATH`, and `LIBS`
- `BUILD_FLAGS` mutations fold into the common compile flags on export
- tuple-shaped `CPPDEFINES` appended in place (`env["CPPDEFINES"].append(("NAME", value))`) export as `-DNAME=value`
- project introspection: `GetBuildType`, `GetProjectOptions`, `GetProjectOption`, and `env.get(key, default)` (falls through env vars → project options → default)
- helper shims: `Dump`, `BoardConfig`, `PioPlatform`, `Flatten`, `VerboseAction`
- non-flag tool/output scopes (e.g. `MKSPIFFSTOOL`, `PROGNAME`, `UPLOAD_PROTOCOL`) — recorded; tool-path scripts don't abort the native build

### Architectural boundaries (neither backend crosses these)

Three categories deliberately fall through to `--platformio`:

1. **Real DAG / incremental rebuilds.** Both backends do a single-pass resolve-then-return. Generated sources must be regenerated each clean build.
2. **Scanner-driven header dep discovery.** fbuild has its own `fbuild-header-scan`; neither harness replicates SCons `CScanner`.
3. **PlatformIO-defined chip-family builders** (`env.MergeFlashImage` for ESP32, `env.PackageJsonFirmware` for OTA, etc.) — lite-SCons records these as `builder_calls` entries; fbuild maps known names to native `fbuild-deploy` ops, otherwise fails fast with a targeted "needs `--platformio` for builder X" message.

### MockEnv-only failure modes (legacy backend)

These are why MockEnv is being retired. The lite backend covers all four:

- `env.Execute(...)` is a no-op — generator scripts that should produce headers at build time silently don't.
- `env.SConscript(...)` hard-fails — recursive build fragments can't compose.
- `env.AddBuildMiddleware(...)` hard-fails — Marlin-class per-source flag tweaks have no path through.
- `env.AddPostAction(...)` is recorded as a note but never fires — OTA `merge_bin` style packagers don't run.

### `build_flags = !python ...` stdout-capture pattern

Out of scope for both backends — only `extra_scripts` entries are interpreted.

## Compile Database (compile_commands.json)

After every build, fbuild generates a [JSON Compilation Database](https://clang.llvm.org/docs/JSONCompilationDatabase.html) so that clangd and VS Code IntelliSense can resolve includes to actual source files.

- Written to both the build directory and the project root (for clangd auto-discovery)
- Uses individual `-I` flags (never `@file` response file references)
- `file` field points to the actual source path, not a build-directory copy
- Cache wrappers (sccache/zccache/ccache) are stripped from compiler paths
- **Library projects** (detected via `library.json` at project root) suppress the project-root copy to avoid overwriting meson/cmake-generated files
