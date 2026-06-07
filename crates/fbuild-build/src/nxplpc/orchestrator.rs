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

use super::{mcu_config, LPC804_LD, LPC804_STARTUP, LPC845_LD, LPC845_STARTUP, MAIN_CPP_SHIM};

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
    std::fs::write(&path, content)?;
    Ok(path)
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

        // 8. Include dirs: sketch + project discovery (libs under lib/, etc.).
        //    No framework include path yet — the bare CMSIS path will gain
        //    the MCUXpresso SDK include dirs when Stage 4 vendors the
        //    framework library.
        let mut include_dirs = vec![ctx.src_dir.clone()];
        pipeline::discover_project_includes(&params.project_dir, &mut include_dirs);

        let compiler = ArmCompiler::new(
            toolchain.get_gcc_path(),
            toolchain.get_gxx_path(),
            &ctx.board.mcu,
            &ctx.board.f_cpu,
            defines,
            include_dirs,
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

        // 10. Run the shared sequential build pipeline.
        //
        // `lib_env = None` because there is no project-as-library path
        // here — the bare LPC8xx target doesn't pre-build dependencies
        // into archives (yet). Stage 4 may add this once an Arduino
        // library ecosystem is in scope.
        pipeline::run_sequential_build_with_libs(
            &compiler,
            &linker,
            ctx,
            params,
            &sources,
            &[],
            None,
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
