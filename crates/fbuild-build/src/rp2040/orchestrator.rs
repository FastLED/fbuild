//! RP2040/RP2350 build orchestrator — wires together config, packages, compiler, linker.
//!
//! Build phases:
//! 1. Parse platformio.ini
//! 2. Load board config (rpipico, rpipico2, etc.)
//! 3. Ensure ARM GCC toolchain
//! 4. Ensure RP2040 cores (arduino-pico by earlephilhower)
//! 5. Setup build directories
//! 6. Scan source files
//! 7. Compile core + variant sources
//! 8. Compile sketch sources
//! 9. Link (with linker script from variant dir)
//! 10. Convert to binary + report size

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use fbuild_core::{Platform, Result};

use crate::compile_database::TargetArchitecture;
use crate::compiler::Compiler as _;
use crate::generic_arm::{ArmCompiler, ArmLinker};
use crate::pipeline;
use crate::{BuildOrchestrator, BuildParams, BuildResult, SourceScanner};

/// RP2040 platform build orchestrator.
pub struct Rp2040Orchestrator;

impl BuildOrchestrator for Rp2040Orchestrator {
    fn platform(&self) -> Platform {
        Platform::RaspberryPi
    }

    fn build(&self, params: &BuildParams) -> Result<BuildResult> {
        let start = Instant::now();

        // 1-2. Parse config, load board, setup build dirs, resolve src dir, collect flags
        let mut ctx = pipeline::BuildContext::new(params)?;

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

        // 4. Ensure RP2040 cores (arduino-pico by earlephilhower)
        let framework = fbuild_packages::library::Rp2040Cores::new(&params.project_dir);
        let framework_dir = fbuild_packages::Package::ensure_installed(&framework)?;
        tracing::info!("RP2040 cores at {}", framework_dir.display());
        let board_id = ctx
            .config
            .get_env_config(&params.env_name)?
            .get("board")
            .cloned()
            .unwrap_or_default();
        let board_props = crate::arduino_props::load_board_props_with_default_menus(
            &framework.get_boards_txt(),
            &board_id,
        );

        // 5. Scan sources (core + variant)
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
            super::mcu_config::get_rp2040_config_for_mcu(&ctx.board.mcu.to_lowercase())?;
        let mut defines = ctx.board.get_defines();
        defines.extend(mcu_config.defines_map());
        add_rp_manifest_defines(&framework_dir, &ctx.board.mcu, &mut defines);
        apply_rp_board_props(&board_props, &framework_dir, &mut defines);
        // Use the resolved core_dir/variant_dir instead of board.get_include_paths():
        // RP2040 board metadata reports `core = earlephilhower`, while the actual
        // package directory is `cores/rp2040/`.
        let mut include_dirs = vec![core_dir.clone(), variant_dir.clone()];
        let framework_include = framework_dir.join("include");
        if framework_include.exists() {
            include_dirs.push(framework_include.clone());
        }
        add_rp_manifest_includes(&framework_dir, &ctx.board.mcu, &mut include_dirs);
        if framework_include.exists() {
            add_rp_family_includes(&framework_include, &ctx.board.mcu, &mut include_dirs);
        }
        add_rp_board_includes(&board_props, &framework_dir, &mut include_dirs);
        include_dirs.push(ctx.src_dir.clone());
        pipeline::discover_project_includes(&params.project_dir, &mut include_dirs);
        // Toolchain sysroot includes
        include_dirs.extend(toolchain.get_include_dirs());
        // Pico SDK includes
        let pico_sdk_dir = framework.get_pico_sdk_dir();
        let pico_sdk_src = pico_sdk_dir.join("src");
        if pico_sdk_src.exists() {
            // Common headers (pico.h, pico/types.h, etc.)
            let common_inc = pico_sdk_src
                .join("common")
                .join("pico_base_headers")
                .join("include");
            if common_inc.exists() {
                include_dirs.push(common_inc);
            }
            // Board headers
            let boards_inc = pico_sdk_src.join("boards").join("include");
            if boards_inc.exists() {
                include_dirs.push(boards_inc);
            }
            // ArduinoCore-API's IPAddress.h pulls in lwIP headers even for
            // non-network sketches, so include the Pico SDK lwIP roots.
            let pico_lwip_inc = pico_sdk_src
                .join("rp2_common")
                .join("pico_lwip")
                .join("include");
            if pico_lwip_inc.exists() {
                include_dirs.push(pico_lwip_inc);
            }
        }
        let lwip_inc = pico_sdk_dir
            .join("lib")
            .join("lwip")
            .join("src")
            .join("include");
        if lwip_inc.exists() {
            include_dirs.push(lwip_inc);
        }

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
        );

        // 7. Create linker (linker script from variant dir)
        let linker_script = framework.get_linker_script(&ctx.board.variant);
        let linker = ArmLinker::new(
            toolchain.get_gcc_path(),
            toolchain.get_ar_path(),
            toolchain.get_objcopy_path(),
            toolchain.get_size_path(),
            linker_script,
            mcu_config,
            params.profile,
            ctx.board.max_flash,
            ctx.board.max_ram,
            params.verbose,
        );

        // 8. Build LibraryBuildEnv for project-as-library compilation
        let gcc_path = toolchain.get_gcc_path();
        let gxx_path = toolchain.get_gxx_path();
        let ar_path = toolchain.get_ar_path();
        let gcc_ar_path = toolchain.get_gcc_ar_path();
        let c_flags = compiler.c_flags();
        let cpp_flags = compiler.cpp_flags();
        // Use gcc-ar for LTO archives so the linker-plugin index is written.
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

        // 9. Run shared sequential build pipeline
        pipeline::run_sequential_build_with_libs(
            &compiler,
            &linker,
            ctx,
            params,
            &sources,
            Some(&lib_env),
            TargetArchitecture::Arm,
            "RP2040",
            start,
        )
    }
}

/// Create an RP2040 orchestrator.
pub fn create() -> Box<dyn BuildOrchestrator> {
    Box::new(Rp2040Orchestrator)
}

/// Check if a project is configured for RP2040.
pub fn is_rp2040_project(project_dir: &Path, env_name: &str) -> bool {
    crate::pipeline::is_platform_project(project_dir, env_name, fbuild_core::Platform::RaspberryPi)
}

fn add_rp_family_includes(
    framework_include: &Path,
    mcu: &str,
    include_dirs: &mut Vec<std::path::PathBuf>,
) {
    let family = if mcu.to_lowercase().starts_with("rp2350") {
        "rp2350"
    } else {
        "rp2040"
    };
    let family_dir = framework_include.join(family);
    if !family_dir.is_dir() {
        return;
    }

    include_dirs.push(family_dir.clone());
    if let Ok(entries) = std::fs::read_dir(&family_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                include_dirs.push(path);
            }
        }
    }
}

fn add_rp_manifest_includes(
    framework_dir: &Path,
    mcu: &str,
    include_dirs: &mut Vec<std::path::PathBuf>,
) {
    for path in rp_manifest_include_files(framework_dir, mcu) {
        add_prefixed_include_file(&path, framework_dir, include_dirs);
    }
}

fn add_rp_manifest_defines(framework_dir: &Path, mcu: &str, defines: &mut HashMap<String, String>) {
    for path in rp_manifest_define_files(framework_dir, mcu) {
        add_define_file(&path, defines);
    }
}

fn rp_manifest_include_files(framework_dir: &Path, mcu: &str) -> Vec<std::path::PathBuf> {
    let family = if mcu.to_lowercase().starts_with("rp2350") {
        "rp2350"
    } else {
        "rp2040"
    };
    vec![
        framework_dir.join("lib").join("core_inc.txt"),
        framework_dir
            .join("lib")
            .join(family)
            .join("platform_inc.txt"),
    ]
}

fn rp_manifest_define_files(framework_dir: &Path, mcu: &str) -> Vec<std::path::PathBuf> {
    let family = if mcu.to_lowercase().starts_with("rp2350") {
        "rp2350"
    } else {
        "rp2040"
    };
    vec![framework_dir
        .join("lib")
        .join(family)
        .join("platform_def.txt")]
}

fn add_prefixed_include_file(
    include_file: &Path,
    base_dir: &Path,
    include_dirs: &mut Vec<std::path::PathBuf>,
) {
    let Ok(content) = std::fs::read_to_string(include_file) else {
        return;
    };

    for line in content.lines() {
        let trimmed = line.trim();
        let rel = trimmed
            .strip_prefix("-iwithprefixbefore/")
            .or_else(|| trimmed.strip_prefix("-iwithprefixbefore"));
        let Some(rel) = rel else {
            continue;
        };
        let path = base_dir.join(rel.trim_start_matches('/'));
        if path.is_dir() {
            include_dirs.push(path);
        }
    }
}

fn add_define_file(define_file: &Path, defines: &mut HashMap<String, String>) {
    let Ok(content) = std::fs::read_to_string(define_file) else {
        return;
    };

    let flags = content
        .lines()
        .flat_map(fbuild_core::shell_split::split)
        .collect::<Vec<_>>();
    apply_define_flags(&flags, defines);
}

fn apply_rp_board_props(
    board_props: &Option<HashMap<String, String>>,
    framework_dir: &Path,
    defines: &mut HashMap<String, String>,
) {
    let Some(props) = board_props.as_ref() else {
        return;
    };
    for key in [
        "usbvid",
        "usbpid",
        "usbpwr",
        "usbstack_flags",
        "variantdefines",
        "led",
    ] {
        if let Some(flags) = props.get(key) {
            let expanded =
                flags.replace("{runtime.platform.path}", &framework_dir.to_string_lossy());
            let tokens = fbuild_core::shell_split::split(&expanded);
            apply_define_flags(&tokens, defines);
        }
    }
    if let Some(value) = props.get("usb_manufacturer") {
        defines.insert("USB_MANUFACTURER".to_string(), value.clone());
    }
    if let Some(value) = props.get("usb_product") {
        defines.insert("USB_PRODUCT".to_string(), value.clone());
    }
}

fn add_rp_board_includes(
    board_props: &Option<HashMap<String, String>>,
    framework_dir: &Path,
    include_dirs: &mut Vec<std::path::PathBuf>,
) {
    let Some(props) = board_props.as_ref() else {
        return;
    };
    let Some(flags) = props.get("usbstack_flags") else {
        return;
    };
    let expanded = flags.replace("{runtime.platform.path}", &framework_dir.to_string_lossy());
    for token in fbuild_core::shell_split::split(&expanded) {
        if let Some(path) = token.strip_prefix("-I") {
            let candidate = Path::new(path);
            if candidate.is_dir() {
                include_dirs.push(candidate.to_path_buf());
            }
        }
    }
}

fn apply_define_flags(flags: &[String], defines: &mut HashMap<String, String>) {
    for flag in flags {
        if let Some(def) = flag.strip_prefix("-D") {
            if let Some((key, val)) = def.split_once('=') {
                defines.insert(key.to_string(), val.trim().to_string());
            } else {
                defines.insert(def.trim().to_string(), "1".to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_rp2040_orchestrator_platform() {
        let orch = Rp2040Orchestrator;
        assert_eq!(orch.platform(), Platform::RaspberryPi);
    }

    #[test]
    fn test_add_rp_family_includes_discovers_mcu_specific_subdirs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let rp2040 = tmp.path().join("rp2040");
        std::fs::create_dir_all(rp2040.join("pico_base")).unwrap();
        std::fs::create_dir_all(rp2040.join("hardware_gpio")).unwrap();

        let mut include_dirs = Vec::new();
        add_rp_family_includes(tmp.path(), "rp2040", &mut include_dirs);

        assert!(include_dirs.contains(&rp2040));
        assert!(include_dirs.contains(&rp2040.join("pico_base")));
        assert!(include_dirs.contains(&rp2040.join("hardware_gpio")));
    }

    #[test]
    fn test_add_prefixed_include_file_reads_platformio_manifest() {
        let tmp = tempfile::TempDir::new().unwrap();
        let target = tmp
            .path()
            .join("pico-sdk")
            .join("src")
            .join("rp2_common")
            .join("pico_platform")
            .join("include");
        std::fs::create_dir_all(&target).unwrap();

        let manifest = tmp.path().join("core_inc.txt");
        std::fs::write(
            &manifest,
            "-iwithprefixbefore/pico-sdk/src/rp2_common/pico_platform/include\n",
        )
        .unwrap();

        let mut include_dirs = Vec::new();
        add_prefixed_include_file(&manifest, tmp.path(), &mut include_dirs);

        assert_eq!(include_dirs, vec![target]);
    }

    #[test]
    fn test_add_define_file_reads_platformio_manifest() {
        let tmp = tempfile::TempDir::new().unwrap();
        let manifest = tmp.path().join("platform_def.txt");
        std::fs::write(&manifest, "-DTARGET_RP2040\n-DPICO_RP2040=1 \n").unwrap();

        let mut defines = HashMap::new();
        add_define_file(&manifest, &mut defines);

        assert_eq!(defines.get("TARGET_RP2040").map(String::as_str), Some("1"));
        assert_eq!(defines.get("PICO_RP2040").map(String::as_str), Some("1"));
    }
}
