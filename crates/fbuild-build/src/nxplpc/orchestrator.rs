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

use super::{
    mcu_config, ARDUINO_STUB_ASSETS, DEVICE_HEADER_ASSETS, LPC804_LD, LPC804_STARTUP, LPC845_LD,
    LPC845_STARTUP, MAIN_CPP_SHIM,
};

/// Per-MCU asset bundle: linker script + startup `.S`. Selected from
/// the embedded `include_str!` constants based on the board's `mcu`
/// field.
#[derive(Debug)]
struct McuAssets {
    linker_script: &'static str,
    startup_asm: &'static str,
}

fn mcu_assets(mcu: &str) -> Result<McuAssets> {
    match mcu {
        "lpc804" => Ok(McuAssets {
            linker_script: LPC804_LD,
            startup_asm: LPC804_STARTUP,
        }),
        "lpc845" => Ok(McuAssets {
            linker_script: LPC845_LD,
            startup_asm: LPC845_STARTUP,
        }),
        other => Err(FbuildError::ConfigError(format!(
            "unknown NXP LPC8xx MCU '{}'; expected one of: lpc804, lpc845",
            other
        ))),
    }
}

/// Write an embedded asset string to `dir/filename` and return the path.
/// Used to materialise the linker script, startup `.S`, and `main.cpp`
/// shim from `include_str!` blobs into the build dir where the toolchain
/// can consume them.
fn write_asset(dir: &std::path::Path, filename: &str, content: &str) -> Result<PathBuf> {
    let path = dir.join(filename);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, content)?;
    Ok(path)
}

fn write_asset_group(
    dir: &std::path::Path,
    assets: &[(&'static str, &'static str)],
) -> Result<Vec<PathBuf>> {
    assets
        .iter()
        .map(|(filename, content)| write_asset(dir, filename, content))
        .collect()
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

        // 4. Pick per-MCU assets (linker script + startup .S).
        let assets = mcu_assets(&ctx.board.mcu)?;

        // 5. Materialise the embedded assets into the build dir.
        //    - <build_dir>/lpc8xx.ld          → -T flag to the linker
        //    - <build_dir>/startup_<mcu>.S    → compiled like any source
        //    - <build_dir>/lpc8xx_main.cpp    → compiled like any source
        let linker_script_path = write_asset(&ctx.build_dir, "lpc8xx.ld", assets.linker_script)?;
        let startup_path = write_asset(
            &ctx.build_dir,
            &format!("startup_{}.S", ctx.board.mcu),
            assets.startup_asm,
        )?;
        let main_shim_path = write_asset(&ctx.build_dir, "lpc8xx_main.cpp", MAIN_CPP_SHIM)?;
        let stub_paths = write_asset_group(&ctx.build_dir, ARDUINO_STUB_ASSETS)?;
        write_asset_group(&ctx.build_dir, DEVICE_HEADER_ASSETS)?;

        // 6. Scan user sources. No external framework yet (Stage 3 / #479).
        let scanner = SourceScanner::new(&ctx.src_dir, &ctx.src_build_dir);
        let mut sources = scanner.scan_all_filtered(None, None, ctx.source_filter.as_deref())?;

        // The framework-owned files (startup + main shim) get treated as
        // "core" sources so they're compiled and linked exactly like any
        // other dependency. Stage 4 (#487) replaces these with the
        // framework-library extraction when ArduinoCore-LPC8xx ships
        // real implementations.
        sources.core_sources.push(startup_path);
        sources.core_sources.push(main_shim_path);
        for path in stub_paths {
            if matches!(
                path.extension().and_then(|ext| ext.to_str()),
                Some("c" | "cc" | "cpp" | "S" | "s")
            ) {
                sources.core_sources.push(path);
            }
        }

        tracing::info!(
            "sources: {} sketch, {} core (framework shim), {} variant",
            sources.sketch_sources.len(),
            sources.core_sources.len(),
            sources.variant_sources.len(),
        );

        // 7. Build the per-MCU ArmMcuConfig + defines.
        let mcu_config = mcu_config::get_arm_mcu_config(&ctx.board.mcu)?;
        let mut defines = ctx.board.get_defines();
        defines.extend(mcu_config.defines_map());

        // 8. Include dirs: framework stub + device headers + CMSIS core +
        //    sketch/project discovery (libs under lib/, etc.). These fbuild-
        //    owned roots replace the misplaced example-local include tree from
        //    FastLED/FastLED#2988 and flow into build_info.json.
        let build_dir = crate::compiler::absolute_from_cwd(&ctx.build_dir);
        let src_dir = crate::compiler::absolute_from_cwd(&ctx.src_dir);
        let mut include_dirs = vec![
            build_dir.join("arduino_stub"),
            build_dir.join("device_headers"),
            cmsis.get_core_include_dir(),
            src_dir,
        ];
        pipeline::discover_project_includes(&params.project_dir, &mut include_dirs);
        let lib_extra_dirs = ctx.config.get_lib_extra_dirs(&params.env_name)?;
        let extra_library_roots =
            pipeline::discover_extra_library_roots(&params.project_dir, &lib_extra_dirs);
        pipeline::add_extra_library_include_dirs(&extra_library_roots, &mut include_dirs);

        let compiler = ArmCompiler::new(
            toolchain.get_gcc_path(),
            toolchain.get_gxx_path(),
            &ctx.board.mcu,
            &ctx.board.f_cpu,
            defines,
            include_dirs.clone(),
            mcu_config.clone(),
            params.profile,
            params.verbose,
        )
        .with_build_unflags(ctx.build_unflags.clone())
        .with_eh_frame_policy(eh_frame_policy);

        // 9. Linker. Uses the per-MCU linker script we just wrote.
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
        );

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
    fn mcu_assets_dispatch_lpc845() {
        let assets = mcu_assets("lpc845").expect("lpc845 must be supported");
        assert!(assets.linker_script.contains("LENGTH = 64K"));
        assert!(assets.startup_asm.contains("Reset_Handler"));
    }

    #[test]
    fn mcu_assets_dispatch_lpc804() {
        let assets = mcu_assets("lpc804").expect("lpc804 must be supported");
        assert!(assets.linker_script.contains("LENGTH = 32K"));
        assert!(assets.startup_asm.contains("Reset_Handler"));
    }

    #[test]
    fn mcu_assets_rejects_unknown_mcu() {
        let err = mcu_assets("lpc999").unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("lpc999"));
        assert!(msg.contains("lpc804") && msg.contains("lpc845"));
    }

    #[test]
    fn write_asset_round_trips_through_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let written = write_asset(dir.path(), "demo.txt", "hello\nworld\n").unwrap();
        let read_back = std::fs::read_to_string(&written).unwrap();
        assert_eq!(read_back, "hello\nworld\n");
        assert_eq!(written, dir.path().join("demo.txt"));
    }

    #[test]
    fn main_cpp_shim_calls_setup_and_loop() {
        // Guarantees the embedded shim hasn't been gutted by an unrelated
        // edit — the framework-owned `main()` is the *only* user-visible
        // entry point until #479 ships the ArduinoCore-LPC8xx framework.
        assert!(MAIN_CPP_SHIM.contains("void setup(void)"));
        assert!(MAIN_CPP_SHIM.contains("void loop(void)"));
        assert!(MAIN_CPP_SHIM.contains("int main(void)"));
        assert!(MAIN_CPP_SHIM.contains("setup();"));
        assert!(MAIN_CPP_SHIM.contains("loop();"));
    }
}
