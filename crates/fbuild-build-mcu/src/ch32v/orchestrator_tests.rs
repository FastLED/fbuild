use super::*;

fn tempdir() -> tempfile::TempDir {
    tempfile::TempDir::new_in(fbuild_paths::temp_subdir("fbuild-ch32v-tests")).unwrap()
}

#[test]
fn test_ch32v_orchestrator_platform() {
    let orch = Ch32vOrchestrator;
    assert_eq!(orch.platform(), Platform::Ch32v);
}

#[test]
fn explicit_board_isa_and_abi_override_selected_platform_defaults() {
    let env = std::collections::HashMap::from([
        ("board_build.march".into(), "rv32imac".into()),
        ("board_build.mabi".into(), "ilp32".into()),
    ]);
    assert_eq!(
        preferred_board_value(
            Some(&env),
            "board_build.march",
            Some("rv32ecxw"),
            Some("rv32ec")
        ),
        Some("rv32imac")
    );
    assert_eq!(
        preferred_board_value(
            Some(&env),
            "board_build.mabi",
            Some("ilp32e"),
            Some("ilp32e")
        ),
        Some("ilp32")
    );
    assert_eq!(
        preferred_board_value(None, "board_build.march", Some("rv32ecxw"), Some("rv32ec")),
        Some("rv32ecxw")
    );
    let mut config = crate::ch32v::mcu_config::get_ch32v_config_for_mcu("ch32v003").unwrap();
    crate::ch32v::mcu_config::apply_board_isa_for_toolchain(
        &mut config,
        preferred_board_value(
            Some(&env),
            "board_build.march",
            Some("rv32ecxw"),
            Some("rv32ec"),
        ),
        preferred_board_value(
            Some(&env),
            "board_build.mabi",
            Some("ilp32e"),
            Some("ilp32e"),
        ),
        "riscv-none-embed",
    );
    assert!(
        config
            .compiler_flags
            .common
            .contains(&"-march=rv32imac".into())
    );
    assert!(config.compiler_flags.common.contains(&"-mabi=ilp32".into()));
}

#[test]
fn effective_board_settings_invalidate_fingerprint() {
    let baseline = board_fingerprint_fields(
        Some("rv32ecxw"),
        Some("ilp32e"),
        Some("variant_CH32V003F4.h"),
    );
    let hash = crate::build_fingerprint::stable_hash_json(&baseline).unwrap();
    for changed in [
        board_fingerprint_fields(
            Some("rv32imac"),
            Some("ilp32e"),
            Some("variant_CH32V003F4.h"),
        ),
        board_fingerprint_fields(
            Some("rv32ecxw"),
            Some("ilp32"),
            Some("variant_CH32V003F4.h"),
        ),
        board_fingerprint_fields(
            Some("rv32ecxw"),
            Some("ilp32e"),
            Some("variant_CH32V003J4.h"),
        ),
    ] {
        assert_ne!(
            hash,
            crate::build_fingerprint::stable_hash_json(&changed).unwrap()
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "downloads CH32V platform, compiler, and framework packages"]
async fn pinned_github_platform_builds_ch32v003_with_gcc8() {
    let backend = crate::compile_backend::CompileBackend::start()
        .await
        .expect("compile backend starts");
    crate::compile_backend::install_global(backend);
    let project = tempdir();
    std::fs::create_dir_all(project.path().join("src")).unwrap();
    std::fs::write(
        project.path().join("platformio.ini"),
        "[env:ch32v003]\nplatform = https://github.com/Community-PIO-CH32V/platform-ch32v.git#b7397c29a71101175bfc94f6ab06f9daac336458\nboard = genericCH32V003F4P6\nframework = arduino\n",
    )
    .unwrap();
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/platform/ch32v003/ch32v003.ino"
        ),
        project.path().join("src/ch32v003.ino"),
    )
    .unwrap();
    let build_dir = fbuild_paths::BuildLayout::new(
        project.path().to_path_buf(),
        "ch32v003".into(),
        fbuild_core::BuildProfile::Release,
    )
    .resolve();
    let params = BuildParams {
        project_dir: project.path().to_path_buf(),
        env_name: "ch32v003".into(),
        clean_all: false,
        clean_only: false,
        clean: false,
        profile: fbuild_core::BuildProfile::Release,
        build_dir,
        verbose: false,
        jobs: Some(2),
        generate_compiledb: false,
        compiledb_only: false,
        log_sender: None,
        symbol_analysis: false,
        symbol_analysis_path: None,
        no_timestamp: true,
        src_dir: None,
        pio_env: std::collections::BTreeMap::new(),
        extra_build_flags: Vec::new(),
        watch_set_cache: None,
        bloat_analysis: false,
        caller_path: None,
    };
    let built = Ch32vOrchestrator.build(&params).await.unwrap();
    assert!(built.success);
    assert!(built.elf_path.as_ref().is_some_and(|path| path.is_file()));
    let log = built.build_log.into_lines().join("\n");
    assert!(log.contains("CH32V requested: platform=https://github.com/Community-PIO-CH32V/platform-ch32v.git#b7397c29a71101175bfc94f6ab06f9daac336458"));
    assert!(log.contains(
        "CH32V resolved platform: ch32v@1.1.0 (source_ref=b7397c29a71101175bfc94f6ab06f9daac336458"
    ));
    assert!(log.contains("toolchain-riscv-linux/archive/"));
    assert!(log.contains("arduino_core_ch32/archive/"));
    assert!(log.contains("8.2.0"));
}

#[test]
fn test_is_ch32v_project() {
    let tmp = tempdir();
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
    let tmp = tempdir();
    std::fs::write(
        tmp.path().join("platformio.ini"),
        "[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n",
    )
    .unwrap();
    assert!(!is_ch32v_project(tmp.path(), "uno"));
}

#[test]
fn test_validate_ch32v_framework() {
    assert!(validate_ch32v_framework(None).is_ok());
    assert!(validate_ch32v_framework(Some("arduino")).is_ok());
    assert!(validate_ch32v_framework(Some(" arduino ")).is_ok());
    let error = validate_ch32v_framework(Some("noneos-sdk")).unwrap_err();
    assert!(error.to_string().contains("1108"));
}

#[test]
fn test_sysclk_define_uses_series_spelling_and_clock_source() {
    assert_eq!(
        sysclk_define("ch32v203", "144000000L", "hsi+pll").unwrap(),
        (
            "SYSCLK_FREQ_144MHz_HSI".to_string(),
            "144000000".to_string()
        )
    );
    assert_eq!(
        sysclk_define("ch32v003", "48000000L", "hsi+pll").unwrap(),
        ("SYSCLK_FREQ_48MHZ_HSI".to_string(), "48000000".to_string())
    );
}

/// ch32v006 must NOT get ch32v003's uppercase spelling. Its vendor file
/// declares the setter under `SYSCLK_FREQ_48MHz_HSI` but dispatches on
/// `SYSCLK_FREQ_48MHZ_HSI`, so the uppercase form selects a call to an
/// undeclared `SetSysClockTo_48MHZ_HSI` and the core fails to compile.
#[test]
fn test_sysclk_define_ch32v006_uses_lowercase_mhz() {
    assert_eq!(
        sysclk_define("ch32v006", "48000000L", "hsi+pll").unwrap(),
        ("SYSCLK_FREQ_48MHz_HSI".to_string(), "48000000".to_string())
    );
    // HSE was never part of the uppercase carve-out; keep it lowercase too.
    assert_eq!(
        sysclk_define("ch32v006", "24000000L", "hse").unwrap(),
        ("SYSCLK_FREQ_24MHz_HSE".to_string(), "24000000".to_string())
    );
}

#[test]
fn test_sysclk_define_rejects_unsupported_frequency() {
    let error = sysclk_define("ch32v203", "8000000L", "hsi").unwrap_err();
    assert!(error.to_string().contains("supported values"));
}

#[test]
fn test_series_to_system_dir() {
    // CH32V series: last digit replaced with 'x'
    assert_eq!(series_to_system_dir("ch32v003"), "CH32V00x");
    assert_eq!(series_to_system_dir("ch32v006"), "CH32VM00X");
    assert_eq!(series_to_system_dir("ch32v103"), "CH32V10x");
    assert_eq!(series_to_system_dir("ch32v203"), "CH32V20x");
    assert_eq!(series_to_system_dir("ch32v303"), "CH32V30x");
    assert_eq!(series_to_system_dir("ch32v307"), "CH32V30x");
    // CH32L follows the same family-directory pattern as CH32V.
    assert_eq!(series_to_system_dir("ch32l103"), "CH32L10x");
    // CH32X: exact uppercase name
    assert_eq!(series_to_system_dir("ch32x035"), "CH32X035");
}

#[test]
fn test_resolve_variant_dir_falls_back_to_family_variant() {
    let tmp = tempdir();
    let fallback = tmp
        .path()
        .join("variants")
        .join("CH32V00x")
        .join("CH32V003F4");
    std::fs::create_dir_all(&fallback).unwrap();

    let resolved = resolve_variant_dir(tmp.path(), "CH32V00x/CH32V006K8", "CH32V00x");
    assert_eq!(resolved, fallback);
}

#[test]
fn test_resolve_variant_h_ignores_missing_preferred_header() {
    let tmp = tempdir();
    std::fs::write(tmp.path().join("variant_CH32V003F4.h"), "").unwrap();

    let resolved = resolve_variant_h(tmp.path(), Some("variant_CH32V006K8.h"));
    assert_eq!(resolved.as_deref(), Some("variant_CH32V003F4.h"));
}
