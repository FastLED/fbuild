---
name: code-review
description: End-of-session code review checking for hardcoded values, core vs platform-specific placement, board JSON quality, and bugs in outstanding changes.
allowed-tools: Bash Read Grep Glob Agent
---

# End-of-Session Code Review

You are performing a code review of all changes made during this session. Review the diff of all uncommitted changes and recent commits on this branch.

## How to gather changes

```bash
# Uncommitted changes (staged + unstaged)
git diff HEAD

# If the working tree is clean, review the most recent commits on this branch
git log --oneline -10
git diff HEAD~5..HEAD  # adjust range as needed
```

Review every changed file. For each file, apply ALL checks below.

---

## Check 1: No Hardcoded Values in Rust Source

**Rule**: Configuration values, magic numbers, board-specific data, and hardware constants MUST live in JSON data files — NOT as literals in `.rs` source code. This project has a two-tier config system: board JSONs (`crates/fbuild-config/assets/boards/json/`) and embedded MCU config JSONs (`crates/fbuild-build/src/{platform}/configs/`). Use them.

**Historical bugs this catches** (commits 17ef7cd, 7a8c7ba):
- Teensy defines were hardcoded in Rust instead of teensy31.json — moved to JSON `defines` array
- ESP32 frequency conversion used a hardcoded lookup table instead of arithmetic
- ESP8266 flash mode was hardcoded as `"dio"` instead of reading from MCU config defaults
- ESP32-C2 had wrong default frequency (80m instead of 60m) because it was copy-pasted from ESP32

**What to flag**:
- Clock frequencies (e.g., `240000000`, `80000000`, `16000000`) outside of `const` declarations
- Baud rates (e.g., `115200`, `460800`, `921600`) not from board JSON `upload.speed`
- Flash frequencies/modes (e.g., `"40m"`, `"80m"`, `"dio"`, `"qio"`) not from config
- Board names or MCU names used for conditional logic instead of config-driven dispatch
- `-D` defines or compiler flags as string literals instead of from MCU config JSON
- Memory addresses, flash sizes, or pin numbers as magic numbers
- Frequency conversion tables (should be arithmetic or config-driven)
- Copy-pasted defaults from one platform applied to another

**What is OK**:
- Named `const` declarations with descriptive names
- Default values in `Default` impls or builder patterns
- Test fixtures and test data
- Log messages, error messages, format strings
- Standard protocol values (e.g., `0xFF` for padding)
- Arithmetic on config-loaded values (e.g., `hz / 1_000_000`)

---

## Check 2: Code Belongs in Core, Not Platform-Specific Crates

**Rule**: Logic that is not specific to a particular hardware platform should live in `fbuild-core` or shared modules in `fbuild-build`, not in individual platform directories.

**Historical bugs this catches** (commits fa142d7, 2813044, 14cc101):
- `compile_with()` was duplicated in every compiler — extracted to `crate::compiler::compile_source()`
- `windows_temp_dir()` was in a platform crate — moved to `fbuild_core::response_file`
- Build orchestration logic (discovery, compilation, linking, results) was duplicated 400+ lines per platform — extracted to `pipeline.rs` with `BuildContext` and `run_sequential_build()`
- Optimization flags were in common linker flags — moved to profile-specific configs
- Flag building helpers (`build_c_flags`, `build_cpp_flags`) were duplicated — centralized

**What to flag**:
- Utility functions in `crates/fbuild-build/src/{platform}/` that have no platform-specific imports
- Data structures or parsing logic that could be reused across platforms
- Error types or trait definitions duplicated across platform modules
- Anything in a platform directory that doesn't reference platform-specific APIs, toolchain binaries, or MCU-specific config
- Helper functions for path manipulation, file discovery, or string processing in platform code
- Frequency conversion, flag merging, or profile selection logic duplicated across platforms

**What is OK**:
- Platform-specific implementations of shared traits (Compiler, Linker, BuildOrchestrator)
- Glue code wiring core logic to platform APIs
- Platform-specific error variants
- MCU config deserialization (each platform has its own JSON schema)

---

## Check 3: Board JSON Quality

**Rule**: Board JSON files must be complete and consistent. Missing or incorrect fields cause silent build failures that are hard to diagnose.

**Historical bugs this catches** (commits 8a34526, c259f6f, 861c599, be3f6ed):
- ESP8266 board was missing critical `defines` array (ESP8266, LWIP, MMU flags) — builds compiled but linked wrong
- ESP32-C2/C61 needed separate MCU skeleton packages not handled by default — builds failed at link
- Teensy 31 was missing FPU flags (`-mfloat-abi=hard`, `-mfpu=fpv4-sp-d16`) — produced wrong code for Cortex-M4F
- ESP32-H2 needed `f_image` field because its `f_flash` value wasn't valid for esptool
- Toolchain sysroot includes were missing from orchestrators — headers not found

**Required fields for ALL boards** (check `crates/fbuild-config/assets/boards/json/`):
- `id` — must match filename (without `.json`)
- `name` — human-readable display name
- `mcu` — uppercase MCU name (e.g., `"ESP32"`, `"ATMEGA328P"`)
- `platform` — PlatformIO platform string (e.g., `"espressif32"`, `"atmelavr"`)
- `frameworks` — at least `["arduino"]`
- `fcpu` — numeric Hz (e.g., `240000000`), NOT a string
- `ram` and `rom` — numeric bytes
- `build.core`, `build.mcu`, `build.f_cpu`, `build.variant`
- `build.extra_flags` — must include `-DARDUINO_<BOARD_DEFINE>`
- `upload.protocol` and `upload.speed`

**Additional required fields by platform**:
- **ESP32**: `build.f_flash`, `build.flash_mode`, `build.arduino.ldscript`, `build.arduino.partitions`
- **ESP32 with PSRAM**: `build.arduino.memory_type` (e.g., `"qio_opi"`)
- **ESP32-H2**: `build.f_image` when `f_flash` is not a valid esptool frequency
- **Teensy ARM**: FPU flags in MCU config if Cortex-M4F/M7
- **AVR**: `build.arduino.ldscript` or linker script path

**f_cpu must end with `"L"` suffix** in the `build` section (e.g., `"240000000L"`)

**What to flag**:
- Missing required fields in new or modified board JSONs
- `fcpu` as string instead of number (or vice versa for `build.f_cpu`)
- `id` field not matching the filename
- Missing `extra_flags` `-DARDUINO_` define
- ESP32 boards without `f_flash`/`flash_mode`
- Copy-pasted board JSON with wrong MCU, frequencies, or memory sizes from the source board

---

## Check 4: MCU Config JSON Quality

**Rule**: Embedded MCU config JSONs (`crates/fbuild-build/src/{platform}/configs/`) drive the entire compilation. Errors here cause subtle build failures.

**Historical bugs this catches** (commits 7a8c7ba, 8a34526, 14cc101):
- ESP32-C2 `default_flash_freq` was `"80m"` (copy-pasted from ESP32) instead of `"60m"`
- ESP8266 MCU config was missing `defines` array entirely — compiler ran without critical defines
- Optimization flags were in `compiler_flags.common` instead of `profiles.release` — couldn't switch profiles

**What to flag**:
- Missing `defines` array in MCU config (especially for new platforms)
- Wrong default frequencies (copy-paste from another MCU config)
- Optimization flags (`-Os`, `-O2`, `-flto`) in `compiler_flags.common` instead of `profiles`
- Architecture-specific flags (e.g., `-march=rv32imc`) that don't match the MCU
- Missing `esptool` section for ESP32 variants
- Missing `profiles.release` and `profiles.quick`
- Linker flags missing `-nostartfiles` for bare-metal platforms

---

## Check 5: Platform Orchestrator Completeness

**Rule**: When adding a new platform or modifying an orchestrator, ensure the full build pipeline is covered.

**Historical bugs this catches** (commits 391b857, 68a39f2, 10aa54f, be3f6ed):
- ESP8266 linker was missing `generate_linker_scripts()` — linker script template not preprocessed
- AVR/ESP32/Teensy orchestrators didn't handle projects with sketches at root (no `src/` dir) — Arduino IDE projects failed
- Toolchain sysroot includes were missing from orchestrators — framework headers not found
- Local `lib/` directory wasn't scanned for user libraries — libraries not compiled

**What to flag in orchestrator changes**:
- Missing `toolchain.get_include_dirs()` for sysroot includes
- No fallback from `src/` to project root for source discovery
- Missing local `lib/` directory scanning
- Linker scripts not generated/preprocessed before link step
- Missing MCU-specific skeleton package handling (ESP32-C2, C61 pattern)
- Missing `compile_db.json` generation
- No size reporting after build

---

## Check 6: General Bug Scan

**Rule**: Look for common bugs and correctness issues in all changed code.

**What to flag**:
- `unwrap()` in non-test, non-CLI code (library crates should return Results)
- Silently swallowed errors (catch + continue pattern like `compile_core()` had)
- Off-by-one errors in loops or slicing
- Resource leaks (files, connections, locks not released)
- Race conditions or missing synchronization
- Logic errors (wrong operator, inverted condition, unreachable branches)
- Path handling that doesn't work on Windows (backslash vs forward slash)
- `compile_source()` or similar catching errors per-file and continuing — real errors surface later as misleading messages
- Response file handling that doesn't account for zccache wrapper
- Include path ordering issues (framework includes MUST come before sketch includes)

---

## Output Format

For each issue found, report:

```
### [CHECK_NAME] file_path:line_number
**Severity**: high | medium | low
**Issue**: One-line description
**Suggestion**: How to fix it
```

If a check finds nothing, say so in one line. End with a summary:

```
## Summary
- Hardcoded values: N issues
- Core placement: N issues
- Board JSON quality: N issues
- MCU config quality: N issues
- Orchestrator completeness: N issues
- Bugs: N issues
```

If there are zero issues across all checks, just say "Code review passed - no issues found."
