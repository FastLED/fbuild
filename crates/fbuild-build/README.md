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

Native mode intentionally supports only a narrow subset of PlatformIO `extra_scripts`.
The goal is to preserve ordinary flag and path mutations without trying to emulate all of SCons.

Supported in native mode:

- `pre:` and `post:` script entries
- `Import("env")` in PRE/POST scripts
- `Import("projenv")` in POST scripts only
- `from SCons.Script import DefaultEnvironment` followed by `DefaultEnvironment()` (returns the same mock env as `Import("env")`)
- `Append`, `AppendUnique`, `Prepend`, and `Replace` over `CPPDEFINES`, `CPPPATH`, `CCFLAGS`, `CFLAGS`, `CXXFLAGS`, `ASFLAGS`, `LINKFLAGS`, `LIBPATH`, and `LIBS`
- `BUILD_FLAGS` (PlatformIO's aggregate compile-flag list) — mutations fold into the common compile flags on export
- tuple-shaped `CPPDEFINES` appended in place (`env["CPPDEFINES"].append(("NAME", value))`) export as `-DNAME=value`
- project introspection: `GetBuildType`, `GetProjectOptions`, `GetProjectOption`, and `env.get(key, default)` (falls through env vars → project options → default)
- helper shims such as `Dump`, `BoardConfig`, `PioPlatform`, `Flatten`, `VerboseAction`, and `Execute`
- non-flag tool/output scopes (e.g. `MKSPIFFSTOOL`, `PROGNAME`, `UPLOAD_PROTOCOL`) are **recorded as notes** rather than failing, so tool-path scripts no longer abort the native build

Rejected or out of scope:

- unsupported script prefixes
- PRE scripts that request `projenv`
- mutations on genuinely unknown SCons scopes (anything outside the supported flag scopes and the known-ignored tool scopes)
- unsupported `Import(...)` targets
- builders, middleware, upload/package hooks, and other arbitrary Python-driven build behavior

Unsupported `extra_scripts` behavior fails the native build early with a recommendation to use `--platformio`.

### Structural limitations of the mock

The shim runs scripts against a mock SCons `env`; it does **not** run real SCons. As a result:

1. **Mock scopes start empty.** The mock does not seed `CCFLAGS`/`LINKFLAGS`/etc. with the platform's existing flags. Scripts that read-transform-and-rewrite (e.g. remove a flag the toolchain injected) see an empty list, so removals are no-ops.
2. **No real SCons graph.** Builders, node dependencies, and the SConscript hierarchy do not exist; `SConscript(...)` bails to `--platformio`.
3. **Effectful `Execute` is a no-op.** `env.Execute(...)` / `VerboseAction` do not run external commands — they are recorded as notes. Scripts that generate headers or download artifacts at build time will not have those side effects.
4. **`build_flags = !python ...` stdout-capture pattern is out of scope.** Only `extra_scripts` entries are interpreted, not flag-emitting shell substitutions in `build_flags`.

## Compile Database (compile_commands.json)

After every build, fbuild generates a [JSON Compilation Database](https://clang.llvm.org/docs/JSONCompilationDatabase.html) so that clangd and VS Code IntelliSense can resolve includes to actual source files.

- Written to both the build directory and the project root (for clangd auto-discovery)
- Uses individual `-I` flags (never `@file` response file references)
- `file` field points to the actual source path, not a build-directory copy
- Cache wrappers (sccache/zccache/ccache) are stripped from compiler paths
- **Library projects** (detected via `library.json` at project root) suppress the project-root copy to avoid overwriting meson/cmake-generated files
