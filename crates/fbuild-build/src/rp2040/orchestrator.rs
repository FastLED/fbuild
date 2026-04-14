//! RP2040/RP2350 build orchestrator — wires together config, packages, compiler, linker.
//!
//! Build phases:
//! 1. Parse platformio.ini
//! 2. Load board config (rpipico, rpipico2, etc.)
//! 3. Ensure the arduino-pico-matched pqt-gcc toolchain
//! 4. Ensure RP2040 cores (arduino-pico by earlephilhower)
//! 5. Setup build directories
//! 6. Scan source files
//! 7. Compile core + variant sources
//! 8. Compile sketch sources
//! 9. Link (with linker script from variant dir)
//! 10. Convert to binary + report size

use std::collections::HashMap;
use std::path::{Path, PathBuf};
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

        // 3. Ensure the arduino-pico-matched pqt-gcc toolchain
        let toolchain = fbuild_packages::toolchain::Rp2040PqtToolchain::new(&params.project_dir);
        let toolchain_dir = fbuild_packages::Package::ensure_installed(&toolchain)?;
        tracing::info!("rp2040 pqt-gcc toolchain at {}", toolchain_dir.display());

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
        let sources = scanner.scan_all_filtered(
            Some(&core_dir),
            Some(&variant_dir),
            ctx.source_filter.as_deref(),
        )?;

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
        if let Some(max_flash) = ctx.board.max_flash {
            defines.insert("PICO_FLASH_SIZE_BYTES".to_string(), max_flash.to_string());
        }
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

        // 7. Generate the linker script the same way upstream does.
        let linker_script =
            generate_linker_script(&framework, &ctx.build_dir, &ctx.board, &board_props)?;
        let boot2_object = compile_boot2_object(
            &compiler,
            &framework_dir,
            &ctx.build_dir,
            &ctx.board.mcu,
            board_props
                .as_ref()
                .and_then(|props| props.get("boot2"))
                .map(String::as_str)
                .unwrap_or("boot2_w25q080_2_padded_checksum"),
        )?;
        let mut mcu_config = mcu_config;
        add_rp_linker_flags(&framework_dir, &ctx.board.mcu, &mut mcu_config);
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

        let mut support_link_inputs = rp_support_objects(&framework_dir, &ctx.board.mcu);
        support_link_inputs.push(boot2_object);

        // 9. Run shared sequential build pipeline
        pipeline::run_sequential_build_with_libs(
            &compiler,
            &linker,
            ctx,
            params,
            &sources,
            &support_link_inputs,
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

fn rp_family(mcu: &str) -> &'static str {
    if mcu.to_lowercase().starts_with("rp2350") {
        "rp2350"
    } else {
        "rp2040"
    }
}

fn generate_linker_script(
    framework: &fbuild_packages::library::Rp2040Cores,
    build_dir: &Path,
    board: &fbuild_config::BoardConfig,
    board_props: &Option<HashMap<String, String>>,
) -> Result<PathBuf> {
    let template = framework.get_linker_script(&board.variant, &board.mcu);
    let mut content = std::fs::read_to_string(&template).map_err(|e| {
        fbuild_core::FbuildError::BuildFailed(format!(
            "failed to read RP2040 linker script template {}: {}",
            template.display(),
            e
        ))
    })?;

    let props = board_props.as_ref();
    let flash_length = props
        .and_then(|p| p.get("flash_length"))
        .cloned()
        .or_else(|| board.max_flash.map(|value| value.to_string()))
        .ok_or_else(|| {
            fbuild_core::FbuildError::BuildFailed(format!(
                "RP2040 board '{}' is missing flash_length / maximum_size metadata",
                board.name
            ))
        })?;
    let ram_length = props
        .and_then(|p| p.get("ram_length"))
        .cloned()
        .or_else(|| board.max_ram.map(|value| format!("{}k", value / 1024)))
        .ok_or_else(|| {
            fbuild_core::FbuildError::BuildFailed(format!(
                "RP2040 board '{}' is missing ram_length / maximum_ram_size metadata",
                board.name
            ))
        })?;

    let flash_total = props
        .and_then(|p| p.get("flash_total"))
        .and_then(|value| value.parse::<u64>().ok())
        .or(board.max_flash)
        .unwrap_or(0);
    let flash_base = 0x1000_0000u64;
    let default_flash_end = flash_base.saturating_add(flash_total);
    let default_flash_end = format!("0x{default_flash_end:08x}");

    let substitutions = [
        ("__FLASH_LENGTH__", flash_length),
        (
            "__EEPROM_START__",
            props
                .and_then(|p| p.get("eeprom_start"))
                .cloned()
                .unwrap_or_else(|| default_flash_end.clone()),
        ),
        (
            "__FS_START__",
            props
                .and_then(|p| p.get("fs_start"))
                .cloned()
                .unwrap_or_else(|| default_flash_end.clone()),
        ),
        (
            "__FS_END__",
            props
                .and_then(|p| p.get("fs_end"))
                .cloned()
                .unwrap_or_else(|| default_flash_end.clone()),
        ),
        ("__RAM_LENGTH__", ram_length),
        (
            "__PSRAM_LENGTH__",
            props
                .and_then(|p| p.get("psram_length"))
                .cloned()
                .unwrap_or_else(|| "0x000000".to_string()),
        ),
    ];

    for (needle, replacement) in substitutions {
        content = content.replace(needle, &replacement);
    }

    let output = build_dir.join("memmap_default.ld");
    std::fs::write(&output, content).map_err(|e| {
        fbuild_core::FbuildError::BuildFailed(format!(
            "failed to write generated RP2040 linker script {}: {}",
            output.display(),
            e
        ))
    })?;
    tracing::info!(
        "generated RP2040 linker script at {} from {}",
        output.display(),
        template.display()
    );
    Ok(output)
}

fn compile_boot2_object(
    compiler: &ArmCompiler,
    framework_dir: &Path,
    build_dir: &Path,
    mcu: &str,
    boot2_name: &str,
) -> Result<PathBuf> {
    let boot2_source = framework_dir
        .join("boot2")
        .join(rp_family(mcu))
        .join(format!("{boot2_name}.S"));
    if !boot2_source.exists() {
        return Err(fbuild_core::FbuildError::BuildFailed(format!(
            "RP2040 boot2 source not found: {}",
            boot2_source.display()
        )));
    }

    let boot2_object = build_dir.join("boot2.o");
    let extra_flags = vec![
        "-I".to_string(),
        framework_dir
            .join("pico-sdk")
            .join("src")
            .join(rp_family(mcu))
            .join("hardware_regs")
            .join("include")
            .to_string_lossy()
            .to_string(),
        "-I".to_string(),
        framework_dir
            .join("pico-sdk")
            .join("src")
            .join("common")
            .join("pico_binary_info")
            .join("include")
            .to_string_lossy()
            .to_string(),
    ];
    let mut boot2_flags = compiler.c_flags();
    boot2_flags.retain(|flag| {
        !matches!(
            flag.as_str(),
            "-flto" | "-fuse-linker-plugin" | "-fno-fat-lto-objects"
        )
    });
    let result = crate::compiler::Compiler::compile_one(
        compiler,
        compiler.gcc_path(),
        &boot2_source,
        &boot2_object,
        &boot2_flags,
        &extra_flags,
    )?;
    if !result.success {
        return Err(fbuild_core::FbuildError::BuildFailed(format!(
            "RP2040 boot2 compile failed for {}:\n{}",
            boot2_source.display(),
            result.stderr
        )));
    }
    Ok(boot2_object)
}

fn add_rp_linker_flags(
    framework_dir: &Path,
    mcu: &str,
    mcu_config: &mut crate::generic_arm::ArmMcuConfig,
) {
    let family = rp_family(mcu);
    let mut extra_flags = vec![
        format!(
            "@{}",
            framework_dir
                .join("lib")
                .join(family)
                .join("platform_wrap.txt")
                .display()
        ),
        format!(
            "@{}",
            framework_dir.join("lib").join("core_wrap.txt").display()
        ),
        "-u_printf_float".to_string(),
        "-u_scanf_float".to_string(),
        "-Wl,--no-warn-rwx-segments".to_string(),
        "-Wl,--check-sections".to_string(),
        "-Wl,--unresolved-symbols=report-all".to_string(),
        "-Wl,--warn-common".to_string(),
        "-Wl,--undefined=runtime_init_install_ram_vector_table".to_string(),
        "-Wl,--undefined=__pre_init_runtime_init_clocks".to_string(),
        "-Wl,--undefined=__pre_init_runtime_init_bootrom_reset".to_string(),
        "-Wl,--undefined=__pre_init_runtime_init_early_resets".to_string(),
        "-Wl,--undefined=__pre_init_runtime_init_usb_power_down".to_string(),
        "-Wl,--undefined=__pre_init_runtime_init_post_clock_resets".to_string(),
        "-Wl,--undefined=__pre_init_runtime_init_spin_locks_reset".to_string(),
        "-Wl,--undefined=__pre_init_runtime_init_boot_locks_reset".to_string(),
        "-Wl,--undefined=__pre_init_runtime_init_bootrom_locking_enable".to_string(),
        "-Wl,--undefined=__pre_init_runtime_init_mutex".to_string(),
        "-Wl,--undefined=__pre_init_runtime_init_default_alarm_pool".to_string(),
        "-Wl,--undefined=__pre_init_first_per_core_initializer".to_string(),
        "-Wl,--undefined=__pre_init_runtime_init_per_core_bootrom_reset".to_string(),
        "-Wl,--undefined=__pre_init_runtime_init_per_core_h3_irq_registers".to_string(),
        "-Wl,--undefined=__pre_init_runtime_init_per_core_irq_priorities".to_string(),
        "-Wl,--start-group".to_string(),
    ];
    mcu_config.linker_flags.append(&mut extra_flags);
    mcu_config.linker_libs.push("-Wl,--end-group".to_string());
}

fn rp_support_objects(framework_dir: &Path, mcu: &str) -> Vec<PathBuf> {
    let family = rp_family(mcu);
    let lib_dir = framework_dir.join("lib").join(family);
    let mut objects = vec![
        lib_dir.join("ota.o"),
        lib_dir.join("libpico.a"),
        lib_dir.join("libipv4.a"),
        lib_dir.join("libbearssl.a"),
    ];
    objects.retain(|path| path.exists());
    objects
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

    #[test]
    fn test_generate_linker_script_substitutes_family_values() {
        let tmp = tempfile::TempDir::new().unwrap();
        let build_dir = tmp.path().join("build");
        std::fs::create_dir_all(&build_dir).unwrap();
        let framework = fbuild_packages::library::Rp2040Cores::new(tmp.path());
        let mut board =
            fbuild_config::BoardConfig::from_board_id("rpipico", &HashMap::new()).unwrap();
        board.variant = "fbuild-test-rpipico".to_string();
        board.max_flash = Some(2_097_152);
        board.max_ram = Some(262_144);
        let template = framework
            .get_variant_dir(&board.variant)
            .join("memmap_default.ld");
        std::fs::create_dir_all(template.parent().unwrap()).unwrap();
        std::fs::write(
            &template,
            "FLASH=__FLASH_LENGTH__ RAM=__RAM_LENGTH__ FS=__FS_START__-__FS_END__ EEPROM=__EEPROM_START__ PSRAM=__PSRAM_LENGTH__",
        )
        .unwrap();

        let mut props = HashMap::new();
        props.insert("flash_length".to_string(), "2093056".to_string());
        props.insert("ram_length".to_string(), "256k".to_string());
        props.insert("fs_start".to_string(), "270528512".to_string());
        props.insert("fs_end".to_string(), "270528512".to_string());
        props.insert("eeprom_start".to_string(), "270528512".to_string());

        let output = generate_linker_script(&framework, &build_dir, &board, &Some(props)).unwrap();
        let generated = std::fs::read_to_string(output).unwrap();
        assert!(generated.contains("FLASH=2093056"));
        assert!(generated.contains("RAM=256k"));
        assert!(generated.contains("FS=270528512-270528512"));
        assert!(generated.contains("EEPROM=270528512"));
        assert!(generated.contains("PSRAM=0x000000"));
    }
}
