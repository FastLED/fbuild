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
        jobs: Some(2),
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
    real_build("esp32s3").await;
}

async fn real_build(target: &str) {
    let project_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../tests/platform/{target}-idf"))
        .canonicalize()
        .unwrap();
    let tmp = tempfile::TempDir::new().unwrap();
    let build_dir = std::env::var_os("FBUILD_IDF_TEST_BUILD_DIR").map_or_else(
        || tmp.path().join("build"),
        |root| PathBuf::from(root).join(target),
    );
    let mut params = params(project_dir, build_dir);
    params.env_name = format!("{target}-idf");
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
fn validated_targets_are_enabled_and_package_pins_are_never_ignored() {
    let params = params(PathBuf::new(), PathBuf::new());
    let mut env = std::collections::HashMap::new();
    for target in [
        "esp32", "esp32s2", "esp32s3", "esp32c3", "esp32c6", "esp32h2", "esp32c2",
    ] {
        assert!(super::validate(target, &env, &params).is_ok(), "{target}");
    }
    for target in ["esp32c5", "esp32p4"] {
        let error = super::validate(target, &env, &params).unwrap_err();
        assert!(error.to_string().contains(target));
        assert!(error.to_string().contains("not yet supported"));
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
        "[env:esp32s3-idf]\nplatform = espressif32\nboard = esp32-p4-evboard\nframework = espidf\n",
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
    assert!(error.to_string().contains("not yet supported"));
    assert!(build.join("keep").is_file());
}

#[test]
fn sdkconfig_preserves_active_config_and_passes_native_defaults() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project = tmp.path();
    let build = project.join("build");
    std::fs::create_dir(&build).unwrap();
    std::fs::write(
        project.join("sdkconfig.defaults"),
        "CONFIG_LOG_DEFAULT_LEVEL=2\n",
    )
    .unwrap();
    let active = project.join("sdkconfig.demo");
    std::fs::write(&active, "CONFIG_LOG_DEFAULT_LEVEL=3\n").unwrap();
    let env = std::collections::HashMap::new();
    let options = super::project::sdkconfig(project, &build, "demo", &env).unwrap();
    assert!(options.contains(&format!("-DSDKCONFIG={}", active.display())));
    assert!(options.contains(&format!(
        "-DSDKCONFIG_DEFAULTS={}",
        project.join("sdkconfig.defaults").display()
    )));
    assert_eq!(
        std::fs::read_to_string(&active).unwrap(),
        "CONFIG_LOG_DEFAULT_LEVEL=3\n"
    );
    let env = std::collections::HashMap::from([(
        "board_build.esp-idf.sdkconfig_path".into(),
        "custom.config".into(),
    )]);
    let options = super::project::sdkconfig(project, &build, "demo", &env).unwrap();
    assert!(options.contains(&format!(
        "-DSDKCONFIG={}",
        project.join("custom.config").display()
    )));
}

#[test]
fn cmake_cannot_override_the_validated_target_or_sdkconfig() {
    for arg in [
        "-DIDF_TARGET=esp32c3",
        "-DSDKCONFIG:FILEPATH=/tmp/bypass",
        "-GUnix Makefiles",
        "-DPYTHON=/other/python",
        "-DCMAKE_TOOLCHAIN_FILE=/other/toolchain",
    ] {
        let env =
            std::collections::HashMap::from([("board_build.cmake_extra_args".into(), arg.into())]);
        assert!(super::cmake_options(&env, &[]).is_err(), "{arg}");
    }
}

#[test]
fn explicit_native_flags_override_build_flags() {
    let env = std::collections::HashMap::from([(
        "board_build.cmake_extra_args".into(),
        "-DCMAKE_C_FLAGS=-DAPP=1".into(),
    )]);
    let options = super::cmake_options(&env, &[]).unwrap();
    assert_eq!(options, ["-DCMAKE_C_FLAGS=-DAPP=1", "-DCMAKE_CXX_FLAGS="]);
    let options = super::cmake_options(&env, &["-DCONFIG_OTHER=2".into()]).unwrap();
    assert!(options.contains(&"-DCMAKE_C_FLAGS=-DAPP=1".into()));
    assert!(options.contains(&"-DCMAKE_CXX_FLAGS=-DCONFIG_OTHER=2".into()));
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
    let component = std::fs::read_to_string(build.join("project/main/CMakeLists.txt")).unwrap();
    for pattern in ["CONFIGURE_DEPENDS", "*.cc", "*.cxx"] {
        assert!(component.contains(pattern), "{pattern}");
    }
    std::fs::write(src.join("CMakeLists.txt"), "idf_component_register()\n").unwrap();
    super::project::source_root(project, &src, &build).unwrap();
    assert!(!build.join("project/main").exists());
}

#[test]
fn removed_build_flags_clear_both_cached_cmake_overlays() {
    let env = std::collections::HashMap::new();
    let with_flags = super::cmake_options(&env, &["-DAPP=1".into()]).unwrap();
    assert!(with_flags.contains(&"-DCMAKE_C_FLAGS=-DAPP=1".into()));
    let without_flags = super::cmake_options(&env, &[]).unwrap();
    assert_eq!(without_flags, ["-DCMAKE_C_FLAGS=", "-DCMAKE_CXX_FLAGS="]);
}

#[tokio::test]
async fn clean_only_removes_outputs_without_the_clean_flag() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(tmp.path().join("platformio.ini"), "[env:esp32s3-idf]\nplatform = espressif32\nboard = esp32-s3-devkitc-1\nframework = espidf\n").unwrap();
    let build = tmp.path().join("build");
    std::fs::create_dir(&build).unwrap();
    std::fs::write(build.join("firmware.bin"), "old output").unwrap();
    let mut params = params(tmp.path().into(), build.clone());
    params.clean_only = true;
    assert!(!params.clean);
    let result = super::super::Esp32Orchestrator
        .build(&params)
        .await
        .unwrap();
    assert!(result.success);
    assert!(result.firmware_path.is_none());
    assert!(!build.exists());
}

macro_rules! target_build_test {
    ($name:ident, $target:literal) => {
        #[tokio::test]
        #[ignore = "downloads ESP-IDF and cross toolchain (~hundreds of MB); requires uv and network"]
        async fn $name() { real_build($target).await; }
    };
}
target_build_test!(espidf_esp32_real_build, "esp32");
target_build_test!(espidf_s2_real_build, "esp32s2");
target_build_test!(espidf_c3_real_build, "esp32c3");
target_build_test!(espidf_c6_real_build, "esp32c6");
target_build_test!(espidf_h2_real_build, "esp32h2");
target_build_test!(espidf_c2_real_build, "esp32c2");

#[test]
fn partitions_are_a_native_defaults_overlay() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project = tmp.path();
    let build = project.join("build");
    std::fs::create_dir(&build).unwrap();
    std::fs::write(project.join("partitions.csv"), "# native CSV\n").unwrap();
    let env = std::collections::HashMap::from([(
        "board_build.partitions".into(),
        "partitions.csv".into(),
    )]);
    let options = super::project::sdkconfig(project, &build, "demo", &env).unwrap();
    assert!(options.contains(&format!(
        "-DSDKCONFIG_DEFAULTS={}",
        build.join("partitions.defaults").display()
    )));
    let overlay = std::fs::read_to_string(build.join("partitions.defaults")).unwrap();
    assert!(overlay.contains("CONFIG_PARTITION_TABLE_CUSTOM=y"));
    assert!(overlay.contains(&project.join("partitions.csv").display().to_string()));
    assert!(!project.join("sdkconfig.demo").exists());
}

#[test]
fn removed_invented_config_inputs_are_not_consumed() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project = tmp.path();
    std::fs::write(project.join("sdkconfig.fragment"), "CONFIG_EXAMPLE=y\n").unwrap();
    let env = std::collections::HashMap::from([
        ("board_build.sdkconfig_defaults".into(), "missing".into()),
        (
            "board_build.esp-idf.sdkconfig_options".into(),
            "CONFIG_EXAMPLE=y".into(),
        ),
    ]);
    let options = super::project::sdkconfig(project, project, "demo", &env).unwrap();
    assert_eq!(
        options,
        [
            format!("-DSDKCONFIG={}", project.join("sdkconfig.demo").display()),
            "-DSDKCONFIG_DEFAULTS=".into()
        ]
    );
}

#[tokio::test]
#[ignore = "downloads ESP-IDF and Xtensa compiler; runs three real CMake/ninja builds; requires uv and network"]
async fn espidf_real_native_config_and_partition_mutations() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(
        project.join("src/main.c"),
        "#include <stdio.h>\nvoid app_main(void) { puts(\"IDF_MUTATION_SENTINEL\"); }\n",
    )
    .unwrap();
    std::fs::write(project.join("platformio.ini"), "[env:esp32s3-idf]\nplatform=espressif32\nboard=esp32-s3-devkitc-1\nframework=espidf\nboard_build.partitions=custom.csv\n").unwrap();
    let defaults = project.join("sdkconfig.defaults");
    std::fs::write(
        &defaults,
        "CONFIG_FREERTOS_HZ=100\nCONFIG_LOG_DEFAULT_LEVEL_INFO=y\n",
    )
    .unwrap();
    let active = project.join("sdkconfig.esp32s3-idf");
    std::fs::write(&active, "CONFIG_LOG_DEFAULT_LEVEL_WARN=y\n").unwrap();
    let csv = project.join("custom.csv");
    std::fs::write(&csv, "nvs,data,nvs,0x9000,0x6000\nphy_init,data,phy,0xf000,0x1000\nfactory,app,factory,0x10000,1M\n").unwrap();
    let params = params(project.clone(), tmp.path().join("build"));
    let orchestrator = super::super::Esp32Orchestrator;
    assert!(orchestrator.build(&params).await.unwrap().success);
    let header = params.build_dir.join("idf/config/sdkconfig.h");
    let first = std::fs::read_to_string(&header).unwrap();
    assert!(
        first.contains("#define CONFIG_LOG_DEFAULT_LEVEL 2"),
        "active sdkconfig.<env> must win over defaults"
    );
    assert!(first.contains("#define CONFIG_FREERTOS_HZ 100"));
    let partitions = std::fs::read(params.build_dir.join("partitions.bin")).unwrap();
    std::fs::write(
        &defaults,
        "CONFIG_FREERTOS_HZ=250\nCONFIG_LOG_DEFAULT_LEVEL_INFO=y\n",
    )
    .unwrap();
    // IDF defaults apply only to unset values: leave the active log level alone.
    let saved = std::fs::read_to_string(&active).unwrap();
    let unset = saved
        .lines()
        .filter(|line| !line.starts_with("CONFIG_FREERTOS_HZ="))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&active, unset + "\n").unwrap();
    assert!(orchestrator.build(&params).await.unwrap().success);
    let second = std::fs::read_to_string(&header).unwrap();
    assert!(second.contains("#define CONFIG_FREERTOS_HZ 250"));
    assert!(second.contains("#define CONFIG_LOG_DEFAULT_LEVEL 2"));
    std::fs::write(&csv, "nvs,data,nvs,0x9000,0x6000\nphy_init,data,phy,0xf000,0x1000\nfactory,app,factory,0x10000,1536K\n").unwrap();
    assert!(orchestrator.build(&params).await.unwrap().success);
    assert_ne!(
        partitions,
        std::fs::read(params.build_dir.join("partitions.bin")).unwrap()
    );
    println!(
        "native sdkconfig.<env>: LOG_DEFAULT_LEVEL=2 overrides defaults=3; sdkconfig.defaults FREERTOS_HZ 100 -> 250; custom CSV app partition 1M -> 1536K changes partitions.bin"
    );
}
