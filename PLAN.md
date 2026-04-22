# Build Pipeline Normalization Plan

## Context

The three platform orchestrators (AVR: 449 lines, Teensy: 435 lines, ESP32: ~850 lines) duplicate ~300 lines of identical code across config parsing, build directory setup, source scanning, compile loops, compile_commands.json generation, link result handling, and BuildResult assembly. This duplication causes bugs when fixes are applied to one orchestrator but not others (e.g., Teensy hardcoding "release" profile, ESP32 silently swallowing esptool failures). Phase 1 bug fixes are complete — this plan covers the structural refactoring to prevent the class of bugs from recurring.

## Phase 1: Bug Fixes (DONE)

- [x] CI: install esptool for ESP32 boards (`.github/workflows/template_build.yml`)
- [x] ESP32: `convert_firmware()` returns `Err` instead of silent fallback (`esp32_linker.rs`)
- [x] Teensy: compiler/linker respect `BuildProfile` param (`teensy_compiler.rs`, `teensy_linker.rs`)
- [x] AVR: compiler/linker respect `BuildProfile` param (`avr_compiler.rs`, `avr_linker.rs`)
- [x] AVR: moved optimization flags from `common`/`linker_flags` to `profiles` in `avr.json`
- [x] Renamed `BuildResult.hex_path` → `firmware_path` everywhere
- [x] All tests pass, clippy clean

---

## Phase 2: Extract Shared Pipeline Module

### Goal

Create `crates/fbuild-build/src/pipeline.rs` with shared helper functions. Each orchestrator calls these instead of duplicating logic. No trait indirection yet — just functions.

### 2.1: `BuildContext` struct + `BuildContext::new()`

Extract the identical config-parse → board-load → build-dir-setup → src-dir-resolve sequence that appears at the top of every orchestrator.

**Duplicated block** (identical in all 3 orchestrators):
```
parse platformio.ini → load board → create build_log → log banner + board info
→ setup cache → clean if requested → ensure build dirs → resolve src_dir
→ parse user_flags + src_flags → merge all_src_flags
```

**New code in `pipeline.rs`:**
```rust
pub struct BuildContext {
    pub config: PlatformIOConfig,
    pub board: BoardConfig,
    pub build_log: BuildLog,
    pub build_dir: PathBuf,
    pub core_build_dir: PathBuf,
    pub src_build_dir: PathBuf,
    pub src_dir: PathBuf,
    pub user_flags: Vec<String>,
    pub src_flags: Vec<String>,
    pub all_src_flags: Vec<String>,
}

impl BuildContext {
    pub fn new(params: &BuildParams) -> Result<Self> { ... }
}
```

**Lines eliminated from each orchestrator:** ~35

**Files modified:**
- `crates/fbuild-build/src/pipeline.rs` — NEW
- `crates/fbuild-build/src/lib.rs` — add `pub mod pipeline;`
- `crates/fbuild-build/src/avr/orchestrator.rs` — replace lines 37-170 with `BuildContext::new()`
- `crates/fbuild-build/src/teensy/orchestrator.rs` — replace lines 37-187 with `BuildContext::new()`
- `crates/fbuild-build/src/esp32/orchestrator.rs` — replace lines 50-147 with `BuildContext::new()`

### 2.2: `discover_project_includes()`

Extract the lib/ + include/ directory discovery loop (identical in all 3).

```rust
/// Add project's include/ dir and lib/ subdirs to include_dirs.
pub fn discover_project_includes(
    project_dir: &Path,
    include_dirs: &mut Vec<PathBuf>,
)
```

**Lines eliminated from each orchestrator:** ~20

**Files modified:**
- `crates/fbuild-build/src/pipeline.rs`
- All 3 orchestrators

### 2.3: `compile_sources_sequential()`

Extract the sequential compile loop used by AVR and Teensy (identical in both). ESP32 already has `compile_sources_parallel()` in the `parallel` module.

```rust
/// Compile a list of sources sequentially with rebuild detection.
pub fn compile_sources_sequential(
    compiler: &dyn Compiler,
    sources: &[PathBuf],
    build_dir: &Path,
    extra_flags: &[String],
    build_log: &mut BuildLog,
) -> Result<Vec<PathBuf>>
```

**Lines eliminated from each AVR/Teensy orchestrator:** ~50 (core + variant + sketch loops)

**Files modified:**
- `crates/fbuild-build/src/pipeline.rs`
- `crates/fbuild-build/src/avr/orchestrator.rs`
- `crates/fbuild-build/src/teensy/orchestrator.rs`

### 2.4: `compile_local_libraries()`

Extract the local lib/ directory compilation (identical in AVR and Teensy). ESP32 uses the parallel `compile_library_with_jobs()` API instead — it stays as-is.

```rust
/// Compile all libraries in project_dir/lib/ sequentially.
pub fn compile_local_libraries(
    compiler: &dyn Compiler,
    project_dir: &Path,
    build_dir: &Path,
    extra_flags: &[String],
    build_log: &mut BuildLog,
) -> Result<Vec<PathBuf>>
```

**Lines eliminated from each AVR/Teensy orchestrator:** ~50

**Files modified:**
- `crates/fbuild-build/src/pipeline.rs`
- `crates/fbuild-build/src/avr/orchestrator.rs`
- `crates/fbuild-build/src/teensy/orchestrator.rs`

### 2.5: `generate_compile_db()`

Extract compile_commands.json generation (identical pattern in all 3, differs only by TargetArchitecture and whether include_flags are separate).

```rust
/// Generate compile_commands.json for core+variant and sketch sources.
pub fn generate_compile_db(
    gcc_path: &Path,
    gxx_path: &Path,
    c_flags: &[String],
    cpp_flags: &[String],
    include_flags: &[String],   // empty for AVR/Teensy, populated for ESP32
    user_flags: &[String],
    all_src_flags: &[String],
    core_sources: &[PathBuf],   // core + variant combined
    sketch_sources: &[PathBuf],
    core_build_dir: &Path,
    src_build_dir: &Path,
    build_dir: &Path,
    project_dir: &Path,
    arch: TargetArchitecture,
) -> Result<Option<PathBuf>>
```

**Lines eliminated from each orchestrator:** ~30

**Files modified:**
- `crates/fbuild-build/src/pipeline.rs`
- All 3 orchestrators

### 2.6: `handle_link_result()` + `assemble_build_result()`

Extract post-link logging and BuildResult construction (identical in all 3).

```rust
/// Log size report and artifacts from a link result.
pub fn handle_link_result(
    link_result: &LinkResult,
    build_log: &mut BuildLog,
)

/// Assemble the final BuildResult from link output.
pub fn assemble_build_result(
    link_result: LinkResult,
    elapsed: f64,
    platform_label: &str,     // "AVR", "Teensy", "ESP32 (esp32s3)"
    env_name: &str,
    compile_database_path: Option<PathBuf>,
    build_log: BuildLog,
) -> BuildResult
```

**Lines eliminated from each orchestrator:** ~45

**Files modified:**
- `crates/fbuild-build/src/pipeline.rs`
- All 3 orchestrators

### 2.7: `log_toolchain_version()`

Extract the toolchain version logging subprocess call (same pattern in all 3, differs by label).

```rust
/// Log the version of a GCC toolchain by running `gcc -dumpversion`.
pub fn log_toolchain_version(
    gcc_path: &Path,
    label: &str,              // "avr-gcc", "arm-none-eabi-gcc", etc.
    build_log: &mut BuildLog,
)
```

**Lines eliminated from each orchestrator:** ~15

---

## Phase 2 Summary

| Helper | AVR savings | Teensy savings | ESP32 savings |
|--------|-------------|----------------|---------------|
| `BuildContext::new()` | ~35 | ~35 | ~30 |
| `discover_project_includes()` | ~20 | ~20 | ~20 |
| `compile_sources_sequential()` | ~50 | ~35 | N/A (uses parallel) |
| `compile_local_libraries()` | ~50 | ~50 | N/A (uses parallel) |
| `generate_compile_db()` | ~30 | ~30 | ~30 |
| `handle_link_result()` + `assemble_build_result()` | ~45 | ~45 | ~45 |
| `log_toolchain_version()` | ~15 | ~15 | ~15 |
| **Total** | **~245** | **~230** | **~140** |

**Before (Phase 1):** AVR 449 + Teensy 435 + ESP32 ~850 = ~1734 lines
**After Phase 2 (est):** AVR ~200 + Teensy ~200 + ESP32 ~710 + pipeline ~200 = ~1310 lines
**After Phase 3 (actual):** AVR 192 + Teensy 185 + ESP32 1191 + pipeline 485 = ~2053 lines
(ESP32 grew from ~850 due to new features added since plan was written: framework built-in
libs, embed files, bootloader ELF conversion, pioarduino metadata resolution)
**Net reduction in shared code:** ~188 lines from AVR/Teensy, plus guaranteed consistency

---

## Phase 2 Execution Order

Each step is independently testable (`cargo check` + `cargo test` must pass after each).

- [x] **Step 1:** Create `pipeline.rs` with all helpers, update `lib.rs`
- [x] **Step 2:** Migrate AVR orchestrator to use pipeline helpers
- [x] **Step 3:** Migrate Teensy orchestrator to use pipeline helpers
- [x] **Step 4:** Migrate ESP32 orchestrator to use BuildContext + pipeline helpers
- [x] **Step 5-10:** All helpers implemented and used (done as part of Steps 1-4)
- [x] **Step 11:** Full verification: `cargo check + clippy + test --workspace` — all 217 tests pass

---

## Phase 3: Unified Compiler Trait + Sequential Build Runner (DONE)

Instead of a full `PlatformBuild` trait (which would over-abstract given ESP32's divergent flow),
Phase 3 took a pragmatic approach:

### 3.1: Extend `Compiler` trait with `gcc_path`/`gxx_path`/`c_flags`/`cpp_flags`

All 3 platform compilers had identical inherent methods. Moved them to the `Compiler` trait
so `pipeline::run_sequential_build()` can work with `&dyn Compiler` without knowing the
concrete type.

**Files modified:**
- `crates/fbuild-build/src/compiler.rs` — added 4 trait methods
- `crates/fbuild-build/src/avr/avr_compiler.rs` — moved methods to trait impl
- `crates/fbuild-build/src/teensy/teensy_compiler.rs` — moved methods to trait impl
- `crates/fbuild-build/src/esp32/esp32_compiler.rs` — moved methods to trait impl

### 3.2: `run_sequential_build()` template method

Extracted the entire compile→link→result flow into `pipeline::run_sequential_build()`.
Handles: compiledb_only early return, sequential compilation of core/variant/sketch/libs,
compile database generation, linking, and result assembly.

**Lines eliminated from AVR:** ~102 (294 → 192)
**Lines eliminated from Teensy:** ~86 (271 → 185)
**ESP32:** Not applicable — uses parallel compilation, SDK lib compilation, embed files,
bootloader prep. Too many hooks for a shared template. Already uses individual pipeline
helpers from Phase 2.

### Phase 3 Execution

- [x] **Step 1:** Add `gcc_path`/`gxx_path`/`c_flags`/`cpp_flags` to `Compiler` trait
- [x] **Step 2:** Add `run_sequential_build()` to `pipeline.rs`
- [x] **Step 3:** Migrate AVR orchestrator
- [x] **Step 4:** Migrate Teensy orchestrator
- [x] **Step 5:** Full verification: `cargo check + clippy + test` — all 37 tests pass

---

## Verification

After each step:
1. `uv run soldr cargo check --workspace --all-targets`
2. `uv run soldr cargo clippy --workspace --all-targets -- -D warnings`
3. `uv run soldr cargo test --workspace --lib`

After all steps:
4. Push → verify all 11 CI board build workflows pass
5. Verify ESP32-S3 produces `firmware.bin` (the original bug)
6. Verify Teensy/AVR produce `firmware.hex` for both quick and release

---

## Critical Files

| File | Role |
|------|------|
| `crates/fbuild-build/src/pipeline.rs` | NEW — shared pipeline helpers |
| `crates/fbuild-build/src/lib.rs` | Add `pub mod pipeline` |
| `crates/fbuild-build/src/avr/orchestrator.rs` | Migrate to pipeline helpers |
| `crates/fbuild-build/src/teensy/orchestrator.rs` | Migrate to pipeline helpers |
| `crates/fbuild-build/src/esp32/orchestrator.rs` | Migrate to pipeline helpers |

## Existing Infrastructure to Reuse

| Module | Functions | File |
|--------|-----------|------|
| `build_output` | `create_build_log()`, `log_build_banner()`, `log_board_info()`, `log_compiling()`, `log_linking()`, `collect_warnings()`, `log_size_report()`, `log_artifact()` | `crates/fbuild-build/src/build_output.rs` |
| `compiler` | `CompilerBase::needs_rebuild()`, `CompilerBase::object_path()`, `Compiler` trait | `crates/fbuild-build/src/compiler.rs` |
| `compile_database` | `generate_entries()`, `CompileDatabase`, `TargetArchitecture` | `crates/fbuild-build/src/compile_database.rs` |
| `source_scanner` | `SourceScanner::scan_all()`, `SourceCollection` | `crates/fbuild-build/src/source_scanner.rs` |
| `linker` | `Linker::link_all()`, `LinkResult` | `crates/fbuild-build/src/linker.rs` |
| `parallel` | `compile_sources_parallel()` | `crates/fbuild-build/src/parallel/mod.rs` |
| `fbuild-packages` | `Cache::new()`, `ensure_build_directories()`, `get_build_dir()` | `crates/fbuild-packages/src/cache.rs` |
| `fbuild-config` | `PlatformIOConfig::from_path()`, `BoardConfig::from_board_id()` | `crates/fbuild-config/src/` |
