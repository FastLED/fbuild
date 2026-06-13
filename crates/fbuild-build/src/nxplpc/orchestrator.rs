//! NXP LPC8xx build orchestrator — Stage 2 of #487.
//!
//! Compiles user sketch sources (.ino → .cpp + .c + .cpp + .S) together
//! with the per-MCU startup `.S` and the hand-rolled Arduino `main.cpp`
//! shim, links against the per-MCU linker script, and emits `firmware.elf`
//! + `firmware.bin` via objcopy.
//!
//! No external framework is required at this stage — the test fixtures
//! (`tests/platform/lpc845/lpc845.ino`,
//! `tests/platform/lpc804/lpc804.ino`) are 3-line `setup()`/`loop()` stubs.
//! Stage 3 (#479) replaces the embedded shim with the framework-owned
//! `main()` from [`zackees/ArduinoCore-LPC8xx`](https://github.com/zackees/ArduinoCore-LPC8xx).
//!
//! Pattern mirrors the Apollo3 orchestrator
//! (`crates/fbuild-build/src/apollo3/orchestrator.rs`) — same Cortex-M
//! family, same `generic_arm::ArmCompiler` + `ArmLinker` pipeline — minus
//! the mbed-os framework machinery that Apollo3 needs.

use std::path::PathBuf;
use std::time::Instant;

use fbuild_core::{FbuildError, Platform, Result};

use crate::compile_database::TargetArchitecture;
use crate::generic_arm::{ArmCompiler, ArmLinker};
use crate::pipeline;
use crate::{BuildOrchestrator, BuildParams, BuildResult, SourceScanner};

use super::mcu_config;

fn board_lpc_family(board: &fbuild_config::BoardConfig) -> Result<&'static str> {
    let mut candidates = vec![
        board.mcu.as_str(),
        board.variant.as_str(),
        board.board.as_str(),
    ];
    if let Some(ldscript) = board.ldscript.as_deref() {
        candidates.push(ldscript);
    }
    for candidate in candidates {
        let lower = candidate.to_ascii_lowercase();
        if lower.contains("lpc804") {
            return Ok("lpc804");
        }
        if lower.contains("lpc845") {
            return Ok("lpc845");
        }
    }
    Err(FbuildError::ConfigError(format!(
        "unknown NXP LPC8xx board '{}' (mcu '{}', variant '{}'); expected LPC804 or LPC845 metadata",
        board.name, board.mcu, board.variant
    )))
}

/// Enumerate compilable translation units (`.c/.cc/.cpp/.S/.s`) directly
/// inside `dir`. Pulls the vendored ArduinoCore-LPC8xx core sources into the
/// build as "core" sources.
fn collect_compilable_sources(dir: &std::path::Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| {
        FbuildError::BuildFailed(format!("failed to read core dir {}: {}", dir.display(), e))
    })? {
        let path = entry
            .map_err(|e| FbuildError::BuildFailed(format!("core dir entry error: {}", e)))?
            .path();
        if path.is_file()
            && matches!(
                path.extension().and_then(|ext| ext.to_str()),
                Some("c" | "cc" | "cpp" | "S" | "s")
            )
        {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

/// NXP LPC8xx (Cortex-M0+) build orchestrator.
pub struct NxpLpcOrchestrator;

impl BuildOrchestrator for NxpLpcOrchestrator {
    fn platform(&self) -> Platform {
        Platform::NxpLpc
    }

    fn build(&self, params: &BuildParams) -> Result<BuildResult> {
        let start = Instant::now();

        // 1-2. Parse platformio.ini, load board, setup build dirs.
        let mut ctx = pipeline::BuildContext::new(params)?;

        // eh_frame strip policy — same convention every other orchestrator
        // follows (#244).
        let eh_frame_policy =
            crate::eh_frame_policy_compute::compute_eh_frame_policy(&ctx, params.profile, None);

        // 3. Ensure ARM GCC. `install_deps` already pre-installs this when
        // the platform is dispatched, but ensure_installed is idempotent
        // and cheap when the toolchain is already on disk.
        let toolchain = fbuild_packages::toolchain::ArmToolchain::new(&params.project_dir);
        let toolchain_dir = fbuild_packages::Package::ensure_installed(&toolchain)?;
        tracing::info!("arm-none-eabi-gcc toolchain at {}", toolchain_dir.display());

        let cmsis = fbuild_packages::library::CmsisFramework::new(&params.project_dir);
        let cmsis_dir = fbuild_packages::Package::ensure_installed(&cmsis)?;
        tracing::info!("CMSIS framework at {}", cmsis_dir.display());

        use fbuild_packages::Toolchain;
        pipeline::log_toolchain_version(
            &toolchain.get_gcc_path(),
            "arm-none-eabi-gcc",
            &mut ctx.build_log,
        );

        // 4. Vendor the Arduino LPC8xx core framework. This supersedes the
        //    embedded `arduino_stub/` shim (FastLED/fbuild#479, #487): the
        //    framework owns `main()`, startup + vector table, wiring,
        //    HardwareSerial, SPI, and the device headers.
        let core = fbuild_packages::library::ArduinoCoreLpc8xx::new(&params.project_dir);
        let core_root = fbuild_packages::Package::ensure_installed(&core)?;
        tracing::info!("ArduinoCore-LPC8xx at {}", core_root.display());

        // 5. Family + linker script. The board's `ldscript` is relative to
        //    the framework package root (e.g.
        //    `linker_scripts/gcc/lpc845_flash.ld`); fall back to the
        //    per-family default when the board omits it.
        let lpc_family = board_lpc_family(&ctx.board)?;
        let ldscript_rel = ctx
            .board
            .ldscript
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("linker_scripts/gcc/{}_flash.ld", lpc_family));
        let linker_script_path = core.linker_script(&ldscript_rel);
        if !linker_script_path.is_file() {
            return Err(FbuildError::ConfigError(format!(
                "ArduinoCore-LPC8xx linker script not found: {} (board ldscript '{}')",
                linker_script_path.display(),
                ldscript_rel
            )));
        }

        // 6. Scan user sources, then add the vendored core sources
        //    (framework main(), startup, wiring, HardwareSerial, SPI, ...)
        //    plus the board variant glue as "core" sources.
        let scanner = SourceScanner::new(&ctx.src_dir, &ctx.src_build_dir);
        let mut sources = scanner.scan_all_filtered(None, None, ctx.source_filter.as_deref())?;

        for path in collect_compilable_sources(&core.core_dir())? {
            sources.core_sources.push(path);
        }
        // The board variant.cpp pulls its base variant in via a relative
        // include, so compiling the board's translation unit is sufficient.
        let variant_cpp = core.variant_dir(&ctx.board.variant).join("variant.cpp");
        if variant_cpp.is_file() {
            sources.core_sources.push(variant_cpp);
        }

        tracing::info!(
            "sources: {} sketch, {} core (ArduinoCore-LPC8xx), {} variant",
            sources.sketch_sources.len(),
            sources.core_sources.len(),
            sources.variant_sources.len(),
        );

        // 7. Build the per-MCU ArmMcuConfig + defines.
        let mcu_config = mcu_config::get_arm_mcu_config(lpc_family)?;
        let mut defines = ctx.board.get_defines();
        defines.extend(mcu_config.defines_map());

        // 8. Include dirs: vendored core + board/base variant + CMSIS core +
        //    sketch/project discovery (libs under lib/, etc.).
        //
        //    Project-local override (FastLED/fbuild#479): when the project
        //    ships its own `variants/<variant>/pins_arduino.h` next to
        //    `platformio.ini`, that dir is prepended so its symbols win over
        //    the vendored variant default.
        let src_dir = crate::compiler::absolute_from_cwd(&ctx.src_dir);
        let project_dir_abs = crate::compiler::absolute_from_cwd(&params.project_dir);
        let mut include_dirs: Vec<PathBuf> = Vec::with_capacity(8);
        let project_variant_dir = project_dir_abs.join("variants").join(&ctx.board.variant);
        if project_variant_dir.join("pins_arduino.h").is_file() {
            tracing::info!(
                "nxplpc: using project-local variant include {}",
                project_variant_dir.display()
            );
            include_dirs.push(project_variant_dir);
            // Also expose the parent variants/ dir so that variant-chain
            // includes like `#include "../<base>/variant.h"` resolve.
            include_dirs.push(project_dir_abs.join("variants"));
        }
        include_dirs.extend([
            core.core_dir(),
            core.variant_dir(&ctx.board.variant),
            core.variant_dir(lpc_family),
            cmsis.get_core_include_dir(),
            src_dir,
        ]);
        pipeline::discover_project_includes(&params.project_dir, &mut include_dirs);
        let lib_extra_dirs = ctx.config.get_lib_extra_dirs(&params.env_name)?;
        let extra_library_roots =
            pipeline::discover_extra_library_roots(&params.project_dir, &lib_extra_dirs);
        pipeline::add_extra_library_include_dirs(&extra_library_roots, &mut include_dirs);
        include_dirs.retain(|dir| !dir.as_os_str().is_empty());

        let compiler = ArmCompiler::new(
            toolchain.get_gcc_path(),
            toolchain.get_gxx_path(),
            lpc_family,
            &ctx.board.f_cpu,
            defines,
            include_dirs.clone(),
            mcu_config.clone(),
            params.profile,
            params.verbose,
        )
        .with_build_unflags(ctx.build_unflags.clone())
        .with_eh_frame_policy(eh_frame_policy);

        // 9. Linker. Uses the vendored per-board linker script; `-L` the
        //    framework root so the script's relative
        //    `INCLUDE linker_scripts/gcc/lpc8xx_common.ld` resolves.
        let linker = ArmLinker::new(
            toolchain.get_gcc_path(),
            toolchain.get_ar_path(),
            toolchain.get_objcopy_path(),
            toolchain.get_size_path(),
            linker_script_path,
            mcu_config,
            params.profile,
            ctx.board.max_flash,
            ctx.board.max_ram,
            params.verbose,
        )
        .with_lib_search_dirs(vec![core.install_path()]);

        // 10. Compile extra library roots before the shared pipeline links them.
        let gcc_path = toolchain.get_gcc_path();
        let gxx_path = toolchain.get_gxx_path();
        let ar_path = toolchain.get_ar_path();
        let gcc_ar_path = toolchain.get_gcc_ar_path();
        let c_flags = crate::compiler::Compiler::c_flags(&compiler);
        let cpp_flags = crate::compiler::Compiler::cpp_flags(&compiler);
        let lib_ar_path = pipeline::pick_archiver(&ar_path, &gcc_ar_path, &c_flags, &cpp_flags);
        let lib_env = pipeline::LibraryBuildEnv {
            gcc_path: &gcc_path,
            gxx_path: &gxx_path,
            ar_path: lib_ar_path,
            c_flags: &c_flags,
            cpp_flags: &cpp_flags,
            include_dirs: &include_dirs,
            verbose: params.verbose,
            jobs: crate::parallel::effective_jobs(params.jobs),
            compiler_cache: None,
        };
        let extra_link_inputs =
            pipeline::compile_extra_libraries(&extra_library_roots, &ctx.build_dir, &lib_env)?;

        // 11. Run the shared sequential build pipeline.
        pipeline::run_sequential_build_with_libs(
            &compiler,
            &linker,
            ctx,
            params,
            &sources,
            &extra_link_inputs,
            Some(&lib_env),
            TargetArchitecture::Arm,
            "NXPLPC",
            start,
        )
    }
}

/// Construct a boxed orchestrator for the dispatch table.
pub fn create() -> Box<dyn BuildOrchestrator> {
    Box::new(NxpLpcOrchestrator)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orchestrator_reports_nxplpc_platform() {
        let orch = NxpLpcOrchestrator;
        assert_eq!(orch.platform(), Platform::NxpLpc);
    }

    #[test]
    fn board_lpc_family_accepts_concrete_arduino_boards() {
        let cases = [
            ("lpc845brk", "lpc845"),
            ("lpcxpresso804", "lpc804"),
            ("lpcxpresso845max", "lpc845"),
        ];
        for (board_id, expected) in cases {
            let board = fbuild_config::BoardConfig::from_board_id(
                board_id,
                &std::collections::HashMap::new(),
            )
            .unwrap();
            assert_eq!(board_lpc_family(&board).unwrap(), expected);
        }
    }

    #[test]
    fn collect_compilable_sources_filters_and_sorts() {
        let dir = tempfile::tempdir().expect("tempdir");
        for name in ["b.cpp", "a.c", "startup.S", "header.h", "notes.txt"] {
            std::fs::write(dir.path().join(name), "x").unwrap();
        }
        let found = collect_compilable_sources(dir.path()).unwrap();
        let names: Vec<String> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        // Only translation units, sorted; headers/text excluded.
        assert_eq!(names, vec!["a.c", "b.cpp", "startup.S"]);
    }

    #[test]
    fn collect_compilable_sources_errors_on_missing_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("does-not-exist");
        assert!(collect_compilable_sources(&missing).is_err());
    }
}
