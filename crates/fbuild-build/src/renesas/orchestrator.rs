//! Renesas RA build orchestrator — wires together config, packages, compiler, linker.
//!
//! Build phases:
//! 1. Parse platformio.ini
//! 2. Load board config (uno_r4_wifi, etc.)
//! 3. Ensure ARM GCC toolchain
//! 4. Ensure Renesas cores (ArduinoCore-renesas)
//! 5. Setup build directories
//! 6. Scan source files
//! 7. Compile core sources
//! 8. Compile sketch sources
//! 9. Link (with linker script)
//! 10. Convert to binary + report size

use std::path::{Path, PathBuf};
use std::time::Instant;

use fbuild_core::{Platform, Result};

use crate::compile_database::TargetArchitecture;
use crate::pipeline;
use crate::{BuildOrchestrator, BuildParams, BuildResult, SourceScanner};

use super::renesas_compiler::RenesasCompiler;
use super::renesas_linker::RenesasLinker;

/// Renesas RA platform build orchestrator.
pub struct RenesasOrchestrator;

impl BuildOrchestrator for RenesasOrchestrator {
    fn platform(&self) -> Platform {
        Platform::RenesasRa
    }

    fn build(&self, params: &BuildParams) -> Result<BuildResult> {
        let start = Instant::now();

        // 1-2. Parse config, load board, setup build dirs, resolve src dir, collect flags
        let mut ctx = pipeline::BuildContext::new(
            &params.project_dir,
            &params.env_name,
            params.clean,
            params.profile,
            params.log_sender.clone(),
        )?;

        // 3. Ensure ARM GCC toolchain
        let toolchain = fbuild_packages::toolchain::ArmToolchain::new(&params.project_dir);
        let toolchain_dir = fbuild_packages::Package::ensure_installed(&toolchain)?;
        tracing::info!("arm-gcc toolchain at {}", toolchain_dir.display());

        use fbuild_packages::Toolchain;
        pipeline::log_toolchain_version(
            &toolchain.get_gcc_path(),
            "arm-none-eabi-gcc",
            &mut ctx.build_log,
        );

        // 4. Ensure Renesas cores (ArduinoCore-renesas)
        let framework = fbuild_packages::library::RenesasCores::new(&params.project_dir);
        let framework_dir = fbuild_packages::Package::ensure_installed(&framework)?;
        tracing::info!("Renesas cores at {}", framework_dir.display());

        // 5. Scan sources
        let core_dir = framework.get_core_dir(&ctx.board.core);
        let variant_dir = framework.get_variant_dir(&ctx.board.variant);

        let scanner = SourceScanner::new(&ctx.src_dir, &ctx.src_build_dir);
        let sources = scanner.scan_all(Some(&core_dir), Some(&variant_dir))?;

        tracing::info!(
            "sources: {} sketch, {} core, {} variant",
            sources.sketch_sources.len(),
            sources.core_sources.len(),
            sources.variant_sources.len(),
        );

        // 6. Build include dirs + compiler
        let mcu_config =
            super::mcu_config::get_renesas_config_for_mcu(&ctx.board.mcu.to_lowercase())?;
        let mut defines = ctx.board.get_defines();
        defines.extend(mcu_config.defines_map());
        // Use resolved core_dir/variant_dir instead of get_include_paths() which
        // doesn't account for core_dir overrides.
        let mut include_dirs = vec![core_dir.clone(), variant_dir];
        // Renesas core has headers in subdirectories (tinyusb/, usb/, etc.)
        discover_header_subdirs(&core_dir, &mut include_dirs);
        // FSP includes from variant's includes.txt (bsp_api.h, CMSIS, etc.)
        include_dirs.extend(framework.get_variant_includes(&ctx.board.variant));
        include_dirs.push(ctx.src_dir.clone());
        pipeline::discover_project_includes(&params.project_dir, &mut include_dirs);
        // Toolchain sysroot includes (ARM CMSIS headers, etc.)
        include_dirs.extend(toolchain.get_include_dirs());

        let compiler = RenesasCompiler::new(
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

        // 7. Create linker (resolve linker script from framework variant)
        let linker_script_path = framework.get_linker_script(&ctx.board.variant);
        let linker = RenesasLinker::new(
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

        // 8. Run shared sequential build pipeline
        pipeline::run_sequential_build(
            &compiler,
            &linker,
            ctx,
            params,
            &sources,
            TargetArchitecture::Arm,
            "Renesas RA",
            start,
        )
    }
}

/// Create a Renesas orchestrator (convenience for get_orchestrator dispatch).
pub fn create() -> Box<dyn BuildOrchestrator> {
    Box::new(RenesasOrchestrator)
}

/// Recursively find subdirectories that contain .h files and add them as include dirs.
fn discover_header_subdirs(dir: &Path, include_dirs: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Check if this directory contains any .h files
            if let Ok(children) = std::fs::read_dir(&path) {
                let has_headers = children
                    .flatten()
                    .any(|e| e.path().extension().is_some_and(|ext| ext == "h"));
                if has_headers {
                    include_dirs.push(path.clone());
                }
            }
            // Recurse into subdirectories
            discover_header_subdirs(&path, include_dirs);
        }
    }
}

/// Check if a project is configured for Renesas by reading its platformio.ini.
pub fn is_renesas_project(project_dir: &Path, env_name: &str) -> bool {
    crate::pipeline::is_platform_project(project_dir, env_name, fbuild_core::Platform::RenesasRa)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_renesas_orchestrator_platform() {
        let orch = RenesasOrchestrator;
        assert_eq!(orch.platform(), Platform::RenesasRa);
    }

    #[test]
    fn test_is_renesas_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:uno_r4_wifi]\nplatform = renesas-ra\nboard = uno_r4_wifi\nframework = arduino\n",
        )
        .unwrap();
        assert!(is_renesas_project(tmp.path(), "uno_r4_wifi"));
        assert!(!is_renesas_project(tmp.path(), "uno"));
    }

    #[test]
    fn test_is_not_renesas_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n",
        )
        .unwrap();
        assert!(!is_renesas_project(tmp.path(), "uno"));
    }
}
