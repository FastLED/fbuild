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

## Compile Database (compile_commands.json)

After every build, fbuild generates a [JSON Compilation Database](https://clang.llvm.org/docs/JSONCompilationDatabase.html) so that clangd and VS Code IntelliSense can resolve includes to actual source files.

- Written to both the build directory and the project root (for clangd auto-discovery)
- Uses individual `-I` flags (never `@file` response file references)
- `file` field points to the actual source path, not a build-directory copy
- Cache wrappers (sccache/zccache/ccache) are stripped from compiler paths
- **Library projects** (detected via `library.json` at project root) suppress the project-root copy to avoid overwriting meson/cmake-generated files
