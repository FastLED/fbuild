//! CH32V build orchestrator — wires together config, packages, compiler, linker.
//!
//! Build phases:
//! 1. Parse platformio.ini
//! 2. Load board config (genericCH32V003F4P6, etc.)
//! 3. Ensure RISC-V GCC toolchain
//! 4. Ensure OpenWCH CH32V cores
//! 5. Setup build directories
//! 6. Scan source files
//! 7. Compile core sources
//! 8. Compile sketch sources
//! 9. Link (with linker script)
//! 10. Convert to binary + report size

use std::path::Path;
use std::time::Instant;

use fbuild_core::{Platform, Result};

use crate::compile_database::TargetArchitecture;
use crate::pipeline;
use crate::{BuildOrchestrator, BuildParams, BuildResult, SourceScanner};

use super::ch32v_compiler::Ch32vCompiler;
use super::ch32v_linker::Ch32vLinker;

/// CH32V platform build orchestrator.
pub struct Ch32vOrchestrator;

impl BuildOrchestrator for Ch32vOrchestrator {
    fn platform(&self) -> Platform {
        Platform::Ch32v
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

        // 3. Ensure RISC-V GCC toolchain
        let toolchain = fbuild_packages::toolchain::RiscvToolchain::new(&params.project_dir);
        let toolchain_dir = fbuild_packages::Package::ensure_installed(&toolchain)?;
        tracing::info!("riscv-gcc toolchain at {}", toolchain_dir.display());

        use fbuild_packages::Toolchain;
        pipeline::log_toolchain_version(
            &toolchain.get_gcc_path(),
            "riscv-none-elf-gcc",
            &mut ctx.build_log,
        );

        // 4. Ensure OpenWCH CH32V cores
        let framework = fbuild_packages::library::Ch32vCores::new(&params.project_dir);
        let framework_dir = fbuild_packages::Package::ensure_installed(&framework)?;
        tracing::info!("CH32V cores at {}", framework_dir.display());

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
        // Derive the series from the MCU name (e.g. "ch32v003f4p6" -> "ch32v003")
        let mcu_lower = ctx.board.mcu.to_lowercase();
        let series = mcu_lower
            .find(|c: char| c.is_ascii_digit())
            .map(|digit_start| {
                let after_digits = mcu_lower[digit_start..]
                    .find(|c: char| !c.is_ascii_digit())
                    .map(|pos| digit_start + pos)
                    .unwrap_or(mcu_lower.len());
                mcu_lower[..after_digits].to_string()
            })
            .unwrap_or_else(|| "ch32v003".to_string());
        let mcu_config = super::mcu_config::get_ch32v_config_for_mcu(&series)?;
        let mut defines = ctx.board.get_defines();
        defines.extend(mcu_config.defines_map());
        let mut include_dirs = ctx.board.get_include_paths(&framework_dir);
        include_dirs.push(ctx.src_dir.clone());
        pipeline::discover_project_includes(&params.project_dir, &mut include_dirs);
        // Toolchain sysroot includes
        include_dirs.extend(toolchain.get_include_dirs());

        let compiler = Ch32vCompiler::new(
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
        let linker = Ch32vLinker::new(
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
            TargetArchitecture::Riscv32,
            "CH32V",
            start,
        )
    }
}

/// Create a CH32V orchestrator (convenience for get_orchestrator dispatch).
pub fn create() -> Box<dyn BuildOrchestrator> {
    Box::new(Ch32vOrchestrator)
}

/// Check if a project is configured for CH32V by reading its platformio.ini.
pub fn is_ch32v_project(project_dir: &Path, env_name: &str) -> bool {
    crate::pipeline::is_platform_project(project_dir, env_name, fbuild_core::Platform::Ch32v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ch32v_orchestrator_platform() {
        let orch = Ch32vOrchestrator;
        assert_eq!(orch.platform(), Platform::Ch32v);
    }

    #[test]
    fn test_is_ch32v_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:ch32v003]\nplatform = ch32v\nboard = genericCH32V003F4P6\nframework = arduino\n",
        )
        .unwrap();
        assert!(is_ch32v_project(tmp.path(), "ch32v003"));
        assert!(!is_ch32v_project(tmp.path(), "uno"));
    }

    #[test]
    fn test_is_not_ch32v_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n",
        )
        .unwrap();
        assert!(!is_ch32v_project(tmp.path(), "uno"));
    }
}
