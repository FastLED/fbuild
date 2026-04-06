//! AVR build orchestrator — wires together config, packages, compiler, linker.
//!
//! Build phases:
//! 1. Parse platformio.ini
//! 2. Load board config
//! 3. Ensure avr-gcc toolchain
//! 4. Ensure Arduino AVR core
//! 5. Setup build directories
//! 6. Scan source files
//! 7. Compile all sources
//! 8. Link into firmware.elf
//! 9. Convert to firmware.hex
//! 10. Report size

use std::path::{Path, PathBuf};
use std::time::Instant;

use fbuild_core::{Platform, Result};

use crate::compile_database::TargetArchitecture;
use crate::pipeline;
use crate::{BuildOrchestrator, BuildParams, BuildResult, SourceScanner};

use super::avr_compiler::AvrCompiler;
use super::avr_linker::AvrLinker;

/// AVR platform build orchestrator.
pub struct AvrOrchestrator;

impl BuildOrchestrator for AvrOrchestrator {
    fn platform(&self) -> Platform {
        Platform::AtmelAvr
    }

    fn build(&self, params: &BuildParams) -> Result<BuildResult> {
        let start = Instant::now();

        // 1-2. Parse config, load board, setup build dirs, resolve src dir, collect flags
        let mut ctx = pipeline::BuildContext::new(params)?;

        // 3. Ensure toolchain
        let toolchain = fbuild_packages::toolchain::AvrToolchain::new(&params.project_dir);
        let toolchain_dir = fbuild_packages::Package::ensure_installed(&toolchain)?;
        tracing::info!("avr-gcc toolchain at {}", toolchain_dir.display());

        use fbuild_packages::Toolchain as _;
        pipeline::log_toolchain_version(&toolchain.get_gcc_path(), "avr-gcc", &mut ctx.build_log);

        // 4. Ensure Arduino core
        let (_framework_dir, core_dir, variant_dir) = ensure_avr_framework(
            &params.project_dir,
            &ctx.board.core,
            &ctx.board.variant,
            ctx.board.platform(),
        )?;

        // 5. Scan sources
        let scanner = SourceScanner::new(&ctx.src_dir, &ctx.src_build_dir);
        let sources = scanner.scan_all(Some(&core_dir), Some(&variant_dir))?;

        tracing::info!(
            "sources: {} sketch, {} core, {} variant",
            sources.sketch_sources.len(),
            sources.core_sources.len(),
            sources.variant_sources.len(),
        );

        // 6. Build include dirs + compiler
        let defines = ctx.board.get_defines();
        // Use the resolved core_dir/variant_dir directly — board.get_include_paths()
        // uses the raw board core name which may differ from the actual directory
        // (e.g. MiniCore's core dir is "MCUdude_corefiles", not "MiniCore").
        let mut include_dirs = vec![core_dir.clone(), variant_dir.clone()];
        include_dirs.push(ctx.src_dir.clone());
        pipeline::discover_project_includes(&params.project_dir, &mut include_dirs);
        // Toolchain sysroot includes (avr/io.h, etc.)
        include_dirs.extend(toolchain.get_include_dirs());

        let mcu_config = super::mcu_config::get_avr_config()?;

        let compiler = AvrCompiler::new(
            toolchain.get_gcc_path(),
            toolchain.get_gxx_path(),
            &ctx.board.mcu,
            &ctx.board.f_cpu,
            defines,
            include_dirs,
            mcu_config.clone(),
            params.profile,
            params.verbose,
        );

        // 7. Create linker
        let linker = AvrLinker::new(
            toolchain.get_gcc_path(),
            toolchain.get_ar_path(),
            toolchain.get_objcopy_path(),
            toolchain.get_size_path(),
            &ctx.board.mcu,
            mcu_config,
            params.profile,
            ctx.board.max_flash,
            ctx.board.max_ram,
            params.verbose,
        );

        // 8. Run shared sequential build pipeline
        pipeline::run_sequential_build(
            &compiler,
            &linker,
            ctx,
            params,
            &sources,
            TargetArchitecture::Avr,
            "AVR",
            start,
        )
    }
}

/// Create an AVR orchestrator (convenience for get_orchestrator dispatch).
pub fn create() -> Box<dyn BuildOrchestrator> {
    Box::new(AvrOrchestrator)
}

/// Select and install the correct AVR Arduino framework based on the board's core name.
///
/// Uses the data-driven `avr_frameworks.json` registry to resolve the correct
/// framework package (GitHub URL, version) for any board core.
/// For `AtmelMegaAvr` boards whose core is `"arduino"`, the lookup key is remapped
/// to `"arduino_megaavr"` so they get `ArduinoCore-megaavr` (which contains the
/// megaAVR variants like `nona4809`) instead of `ArduinoCore-avr`.
/// Returns (framework_root, core_dir, variant_dir).
fn ensure_avr_framework(
    project_dir: &Path,
    core_name: &str,
    variant_name: &str,
    platform: Option<fbuild_core::Platform>,
) -> fbuild_core::Result<(PathBuf, PathBuf, PathBuf)> {
    use fbuild_packages::Package;

    // megaAVR boards (e.g. nano_every) share core name "arduino" with standard AVR
    // but need ArduinoCore-megaavr instead of ArduinoCore-avr.
    let lookup_key =
        if platform == Some(fbuild_core::Platform::AtmelMegaAvr) && core_name == "arduino" {
            "arduino_megaavr"
        } else {
            core_name
        };

    let framework = fbuild_packages::library::AvrFramework::for_core(lookup_key, project_dir)?;
    let framework_dir = framework.ensure_installed()?;
    tracing::info!(
        "AVR framework for core '{}' (lookup '{}') at {}",
        core_name,
        lookup_key,
        framework_dir.display()
    );
    let core_dir = framework.get_core_dir(core_name);
    let variant_dir = framework.get_variant_dir(variant_name);
    Ok((framework_dir, core_dir, variant_dir))
}

/// Check if a project is configured for AVR by reading its platformio.ini.
pub fn is_avr_project(project_dir: &Path, env_name: &str) -> bool {
    crate::pipeline::is_platform_project(project_dir, env_name, fbuild_core::Platform::AtmelAvr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_avr_orchestrator_platform() {
        let orch = AvrOrchestrator;
        assert_eq!(orch.platform(), Platform::AtmelAvr);
    }

    #[test]
    fn test_is_avr_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n",
        )
        .unwrap();
        assert!(is_avr_project(tmp.path(), "uno"));
        assert!(!is_avr_project(tmp.path(), "esp32"));
    }

    #[test]
    fn test_is_not_avr_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:esp32]\nplatform = espressif32\nboard = esp32dev\nframework = arduino\n",
        )
        .unwrap();
        assert!(!is_avr_project(tmp.path(), "esp32"));
    }

    /// Verify that megaAVR boards remap "arduino" core to "arduino_megaavr" framework.
    #[test]
    fn test_megaavr_core_remaps_to_megaavr_framework() {
        let core = "arduino";
        let platform = Some(Platform::AtmelMegaAvr);
        let lookup_key = if platform == Some(Platform::AtmelMegaAvr) && core == "arduino" {
            "arduino_megaavr"
        } else {
            core
        };
        assert_eq!(lookup_key, "arduino_megaavr");

        // Standard AVR should NOT remap
        let platform_avr = Some(Platform::AtmelAvr);
        let lookup_avr = if platform_avr == Some(Platform::AtmelMegaAvr) && core == "arduino" {
            "arduino_megaavr"
        } else {
            core
        };
        assert_eq!(lookup_avr, "arduino");
    }
}
