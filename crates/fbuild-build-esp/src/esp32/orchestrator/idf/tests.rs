//! IDF adapter contracts, including a real opt-in CMake/ninja build.
use crate::BuildOrchestrator;
use std::path::PathBuf;

fn params(project_dir: PathBuf, build_dir: PathBuf) -> crate::BuildParams {
    crate::BuildParams {
        project_dir,
        shrink: Default::default(),
        env_name: "esp32s3-idf".into(),
        clean_all: false,
        clean_only: false,
        clean: false,
        profile: fbuild_core::BuildProfile::Release,
        build_dir,
        verbose: false,
        jobs: None,
        generate_compiledb: false,
        compiledb_only: false,
        log_sender: None,
        symbol_analysis: false,
        symbol_analysis_path: None,
        no_timestamp: true,
        src_dir: None,
        pio_env: Default::default(),
        extra_build_flags: Vec::new(),
        watch_set_cache: None,
        bloat_analysis: false,
        caller_path: None,
    }
}

/// Real CMake/ninja boundary: no fake compiler or subprocess.
#[tokio::test]
#[ignore = "downloads ESP-IDF 5.4 and Xtensa toolchain (~hundreds of MB); requires uv and network"]
async fn espidf_s3_real_build() {
    let project_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/platform/esp32s3-idf")
        .canonicalize()
        .unwrap();
    let tmp = tempfile::TempDir::new().unwrap();
    let build_dir = std::env::var_os("FBUILD_IDF_TEST_BUILD_DIR")
        .map_or_else(|| tmp.path().join("build"), PathBuf::from);
    let params = params(project_dir, build_dir);
    let result = super::super::Esp32Orchestrator
        .build(&params)
        .await
        .unwrap();
    assert!(result.success);
    assert!(result.firmware_path.unwrap().is_file());
    let size = result.size_info.unwrap();
    assert!(size.total_flash > 0);
    assert!(size.total_ram < size.max_ram.unwrap());
    println!(
        "IDF build: flash={} RAM={} artifacts={}",
        size.total_flash,
        size.total_ram,
        params.build_dir.display()
    );
    for line in result
        .build_log
        .into_lines()
        .into_iter()
        .rev()
        .take(14)
        .rev()
    {
        println!("{line}");
    }
    for file in [
        "bootloader.bin",
        "partitions.bin",
        "idf/build.ninja",
        "idf/project_description.json",
        "idf/flasher_args.json",
    ] {
        assert!(params.build_dir.join(file).is_file(), "{file}");
    }
    let database = std::fs::read_to_string(params.build_dir.join("compile_commands.json")).unwrap();
    assert!(database.contains("idf_probe/probe.c"));
    assert!(!database.contains("cores/esp32/main.cpp"));
    let config = std::fs::read_to_string(params.build_dir.join("idf/config/sdkconfig.h")).unwrap();
    assert!(config.contains("#define CONFIG_LOG_DEFAULT_LEVEL 2"));
}

#[test]
fn dispatch_keeps_arduino_and_dual_on_the_existing_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    for (framework, expected) in [
        ("espidf", true),
        (" arduino , espidf ", false),
        ("arduino", false),
        ("espidf, arduino", false),
    ] {
        std::fs::write(
            tmp.path().join("platformio.ini"),
            format!("[env:app]\nframework = {framework}\n"),
        )
        .unwrap();
        let config =
            fbuild_config::PlatformIOConfig::from_path(&tmp.path().join("platformio.ini")).unwrap();
        assert_eq!(
            super::requested(&config, "app").unwrap(),
            expected,
            "{framework}"
        );
    }
}

#[test]
fn only_s3_is_enabled_and_package_pins_are_never_ignored() {
    let params = params(PathBuf::new(), PathBuf::new());
    let mut env = std::collections::HashMap::new();
    assert!(super::validate("esp32s3", &env, &params).is_ok());
    for target in [
        "esp32", "esp32s2", "esp32c2", "esp32c3", "esp32c5", "esp32c6", "esp32h2", "esp32p4",
    ] {
        let error = super::validate(target, &env, &params).unwrap_err();
        assert!(error.to_string().contains(target));
        assert!(error.to_string().contains("ESP32-S3 only"));
    }
    env.insert("platform_packages".into(), "framework-espidf@0.0.0".into());
    assert!(
        super::validate("esp32s3", &env, &params)
            .unwrap_err()
            .to_string()
            .contains("platform_packages")
    );
}

#[tokio::test]
async fn unsupported_target_fails_before_clean_or_download() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("platformio.ini"),
        "[env:esp32s3-idf]\nplatform = espressif32\nboard = esp32dev\nframework = espidf\n",
    )
    .unwrap();
    let build = tmp.path().join("build");
    std::fs::create_dir(&build).unwrap();
    std::fs::write(build.join("keep"), "keep").unwrap();
    let mut params = params(tmp.path().into(), build.clone());
    params.clean = true;
    let error = super::super::Esp32Orchestrator
        .build(&params)
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("ESP32-S3 only"));
    assert!(build.join("keep").is_file());
}

#[test]
fn sdkconfig_changes_reseed_but_an_unchanged_input_preserves_idf_output() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project = tmp.path();
    let build = project.join("build");
    std::fs::create_dir(&build).unwrap();
    let mut env = std::collections::HashMap::new();
    env.insert(
        "board_build.esp-idf.sdkconfig_options".into(),
        "CONFIG_LOG_DEFAULT_LEVEL=2".into(),
    );
    let sdkconfig = super::project::sdkconfig(project, &build, "esp32s3", &env, &[]).unwrap();
    std::fs::write(&sdkconfig, "IDF generated output").unwrap();
    super::project::sdkconfig(project, &build, "esp32s3", &env, &[]).unwrap();
    assert_eq!(
        std::fs::read_to_string(&sdkconfig).unwrap(),
        "IDF generated output"
    );
    env.insert(
        "board_build.esp-idf.sdkconfig_options".into(),
        "CONFIG_LOG_DEFAULT_LEVEL=3".into(),
    );
    super::project::sdkconfig(project, &build, "esp32s3", &env, &[]).unwrap();
    assert!(
        std::fs::read_to_string(&sdkconfig)
            .unwrap()
            .contains("CONFIG_LOG_DEFAULT_LEVEL=3")
    );
}

#[test]
fn cmake_cannot_override_the_validated_target_or_sdkconfig() {
    for arg in [
        "-DIDF_TARGET=esp32c3",
        "-DSDKCONFIG:FILEPATH=/tmp/bypass",
        "-GUnix Makefiles",
        "-DCMAKE_C_COMPILER=/other/gcc",
        "-DIDF_PATH=/other/idf",
        "-DCMAKE_MAKE_PROGRAM=/other/ninja",
    ] {
        let env =
            std::collections::HashMap::from([("board_build.cmake_extra_args".into(), arg.into())]);
        assert!(super::cmake_options(&env, &[]).is_err(), "{arg}");
    }
}

#[test]
fn explicit_native_flags_are_preserved_and_conflicts_rejected() {
    let env = std::collections::HashMap::from([(
        "board_build.cmake_extra_args".into(),
        "-DCMAKE_C_FLAGS=-DAPP=1".into(),
    )]);
    let options = super::cmake_options(&env, &[]).unwrap();
    assert_eq!(options, ["-DCMAKE_C_FLAGS=-DAPP=1", "-DCMAKE_CXX_FLAGS="]);
    assert!(super::cmake_options(&env, &["-DOTHER=2".into()]).is_err());
}

#[test]
fn generated_project_never_scans_the_project_root_or_keeps_a_stale_main() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project = tmp.path();
    let build = project.join("build");
    std::fs::create_dir(&build).unwrap();
    assert!(super::project::source_root(project, project, &build).is_err());
    let src = project.join("src");
    std::fs::create_dir(&src).unwrap();
    super::project::source_root(project, &src, &build).unwrap();
    std::fs::write(src.join("CMakeLists.txt"), "idf_component_register()\n").unwrap();
    assert!(
        super::project::source_root(project, &src, &build)
            .unwrap_err()
            .to_string()
            .contains("--clean")
    );
}

#[test]
fn unknown_frameworks_are_not_silently_added_to_idf_support() {
    let params = params(PathBuf::new(), PathBuf::new());
    let env = std::collections::HashMap::from([("framework".into(), "espidf, other".into())]);
    assert!(super::validate("esp32s3", &env, &params).is_err());
}

#[test]
fn removed_build_flags_clear_both_cached_cmake_overlays() {
    let env = std::collections::HashMap::new();
    let with_flags = super::cmake_options(&env, &["-DAPP=1".into()]).unwrap();
    assert!(with_flags.contains(&"-DCMAKE_C_FLAGS=-DAPP=1".into()));
    let without_flags = super::cmake_options(&env, &[]).unwrap();
    assert_eq!(without_flags, ["-DCMAKE_C_FLAGS=", "-DCMAKE_CXX_FLAGS="]);
}
