//! Real UNO R4 build regression for FastLED/fbuild#1671.

use fbuild_build::{BuildOrchestrator, BuildParams, compile_backend};
use fbuild_core::BuildProfile;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "#1671: downloads ARM GCC, Arduino Renesas core and published USB profiles"]
async fn uno_r4_default_core_builds_string_and_ipaddress() {
    tokio::task::spawn_blocking(|| {
        let registry = tempfile::tempdir().unwrap();
        assert!(fbuild_core::usb::profiles::populate_profiles_from_paths(
            &registry.path().join("_meta.json"),
            &registry.path().join("usb-profiles.json"),
        ));
    })
    .await
    .unwrap();
    install_test_compile_backend().await;
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path();
    std::fs::write(
        project.join("platformio.ini"),
        "[env:uno_r4]\nplatform = renesas-ra\nboard = uno_r4_wifi\nframework = arduino\n",
    )
    .unwrap();
    std::fs::create_dir(project.join("src")).unwrap();
    std::fs::write(
        project.join("src/main.cpp"),
        r#"
#include <Arduino.h>
#include <IPAddress.h>
String greeting("UNO R4");
IPAddress address(192, 0, 2, 1);
volatile unsigned long result;
void setup() { result = greeting.length() + address[3]; }
void loop() { result++; }
"#,
    )
    .unwrap();
    let params = BuildParams {
        project_dir: project.to_path_buf(),
        shrink: Default::default(),
        env_name: "uno_r4".into(),
        build_dir: project
            .join(fbuild_paths::FBUILD_DIR_NAME)
            .join(fbuild_paths::BUILD_DIR_NAME)
            .join("uno_r4/release"),
        clean_all: false,
        clean_only: false,
        clean: true,
        profile: BuildProfile::Release,
        verbose: true,
        jobs: Some(4),
        generate_compiledb: true,
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
    };
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(900),
        fbuild_build::renesas::RenesasOrchestrator.build(&params),
    )
    .await
    .expect("UNO R4 build timed out")
    .expect("UNO R4 build must succeed");
    assert!(result.success);
    let elf = result.elf_path.as_ref().expect("firmware ELF");
    assert!(elf.is_file());
    assert!(result.size_info.as_ref().unwrap().total_flash > 0);
    let commands: Vec<serde_json::Value> = serde_json::from_slice(
        &std::fs::read(
            result
                .compile_database_path
                .as_ref()
                .expect("compile database"),
        )
        .unwrap(),
    )
    .unwrap();
    for source in ["String.cpp", "IPAddress.cpp"] {
        assert!(
            commands.iter().any(|command| {
                let file =
                    fbuild_core::path::NormalizedPath::from(command["file"].as_str().unwrap_or(""))
                        .display_slash();
                file.ends_with(&format!("/{source}")) && file.contains("renesas_uno-1.6.0/")
            }),
            "{source} must compile from the default 1.6.0 core"
        );
    }
}

/// A sketch that includes one of the renesas core's bundled libraries
/// (`WiFiS3`, which lives in `libraries/WiFiS3/src/`) must compile and link —
/// before FastLED/fbuild#2020 it died at `fatal error: WiFiS3.h: No such file
/// or directory` because the core's `libraries/` tree never reached the
/// include path and its sources were never compiled.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "#2020: downloads ARM GCC + Arduino Renesas core; builds the real WiFiS3 library"]
async fn uno_r4_wifi_sketch_with_bundled_wifis3_library_builds() {
    tokio::task::spawn_blocking(|| {
        let registry = tempfile::tempdir().unwrap();
        assert!(fbuild_core::usb::profiles::populate_profiles_from_paths(
            &registry.path().join("_meta.json"),
            &registry.path().join("usb-profiles.json"),
        ));
    })
    .await
    .unwrap();
    install_test_compile_backend().await;
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path();
    std::fs::write(
        project.join("platformio.ini"),
        "[env:uno_r4_wifi]\nplatform = renesas-ra\nboard = uno_r4_wifi\nframework = arduino\n",
    )
    .unwrap();
    std::fs::create_dir(project.join("src")).unwrap();
    std::fs::write(
        project.join("src/main.cpp"),
        r#"
#include <WiFiS3.h>
void setup() {
  // Touch a symbol the library must define, so a header-only include cannot
  // pass: the link fails with `undefined reference to WiFiS3Class::begin`.
  WiFi.begin("ssid", "password");
}
void loop() {}
"#,
    )
    .unwrap();
    let params = BuildParams {
        project_dir: project.to_path_buf(),
        shrink: Default::default(),
        env_name: "uno_r4_wifi".into(),
        build_dir: project
            .join(fbuild_paths::FBUILD_DIR_NAME)
            .join(fbuild_paths::BUILD_DIR_NAME)
            .join("uno_r4_wifi/release"),
        clean_all: false,
        clean_only: false,
        clean: true,
        profile: BuildProfile::Release,
        verbose: true,
        jobs: Some(4),
        generate_compiledb: true,
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
    };
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(900),
        fbuild_build::renesas::RenesasOrchestrator.build(&params),
    )
    .await
    .expect("UNO R4 WiFi build timed out")
    .expect("UNO R4 WiFi build must succeed");
    assert!(result.success);
    let elf = result.elf_path.as_ref().expect("firmware ELF");
    assert!(elf.is_file());
    assert!(result.size_info.as_ref().unwrap().total_flash > 0);

    // The bundled library must have been COMPILED, not merely found: at least
    // one of WiFiS3's own translation units is in the compile database.
    let commands: Vec<serde_json::Value> = serde_json::from_slice(
        &std::fs::read(
            result
                .compile_database_path
                .as_ref()
                .expect("compile database"),
        )
        .unwrap(),
    )
    .unwrap();
    let wifis3_sources: Vec<String> = commands
        .iter()
        .map(|command| {
            fbuild_core::path::NormalizedPath::from(command["file"].as_str().unwrap_or(""))
                .display_slash()
        })
        .filter(|file| file.contains("/libraries/WiFiS3/src/"))
        .collect();
    assert!(
        !wifis3_sources.is_empty(),
        "WiFiS3 sources must be compiled from the core's libraries/ tree"
    );
}

/// The orchestrator compiles through the process-wide compile backend
/// (FastLED/fbuild#800), which only the daemon wires at startup — this
/// acceptance pair must install its own.
///
/// Initialized at most once per test process: `renesas_acceptance::` selects
/// both tests in one process, and a second concurrent `CompileBackend::start()`
/// cannot win the zccache cache-root writer slot from the first and fails with
/// "another live daemon already holds this cache root".
async fn install_test_compile_backend() {
    static INSTALL: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    INSTALL
        .get_or_init(|| async {
            let backend = compile_backend::CompileBackend::start()
                .await
                .expect("compile backend starts for the renesas acceptance pair");
            compile_backend::install_global(backend);
        })
        .await;
}
