//! Native configuration and ordinary Arduino sketch-root contracts (#1914).
use fbuild_build::{compile_backend, provision, provision_env};
use fbuild_core::Platform;

fn project(
    root_sketch: bool,
    frequency: Option<&str>,
    limits: Option<(u64, u64)>,
) -> tempfile::TempDir {
    let project = tempfile::tempdir().unwrap();
    let path = project.path();
    std::fs::create_dir(path.join("boards")).unwrap();
    let mut board: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../fbuild-config/assets/boards/json/ci1306.json"
    )))
    .unwrap();
    board["id"] = "ci1306_custom".into();
    if let Some(frequency) = frequency {
        board["build"]["f_cpu"] = frequency.into();
    }
    if let Some((flash, ram)) = limits {
        board["upload"]["maximum_size"] = flash.into();
        board["upload"]["maximum_ram_size"] = ram.into();
    }
    std::fs::write(
        path.join("boards/ci1306_custom.json"),
        serde_json::to_vec_pretty(&board).unwrap(),
    )
    .unwrap();
    std::fs::write(path.join("platformio.ini"),
        "[env:ci1306]\nplatform = ci13xx\nboard = ci1306_custom\nframework = arduino\nbuild_unflags = -flto\nbuild_flags = -fno-lto\n").unwrap();
    let source = if root_sketch {
        path.to_path_buf()
    } else {
        let src = path.join("src");
        std::fs::create_dir(&src).unwrap();
        src
    };
    std::fs::write(
        source.join("main.ino"),
        r#"
volatile unsigned long root_observed;
void setup() { pinMode(2, OUTPUT); root_observed = laterValue(2); }
void loop() { digitalWrite(2, root_observed++ & 1); }
unsigned long laterValue(byte value) { return millis() + value; }
"#,
    )
    .unwrap();
    project
}

#[tokio::test]
#[ignore = "#1914: provisions actual Nuclei GCC/Chipintelli packages for native configuration validation"]
async fn ci1306_rejects_clock_conflicting_with_vendor_recipe() {
    let valid = project(false, None, None);
    let report = provision_env(valid.path(), "ci1306", provision::ProvisionMode::Install)
        .await
        .unwrap();
    assert!(
        !report.failed(),
        "unchanged selected CI1306 configuration must work"
    );
    let incompatible = project(false, Some("200000000L"), None);
    let error = provision_env(
        incompatible.path(),
        "ci1306",
        provision::ProvisionMode::Install,
    )
    .await
    .expect_err("unsupported board clock must not silently compile for another frequency");
    let message = error.to_string();
    assert!(
        message.contains("clock") || message.contains("f_cpu"),
        "{message}"
    );
    assert!(
        message.contains("200000000"),
        "diagnostic must identify requested clock: {message}"
    );
}

async fn build(project: &std::path::Path) -> fbuild_build::BuildResult {
    let backend = compile_backend::CompileBackend::start().await.unwrap();
    compile_backend::install_global(backend);
    tokio::time::timeout(
        std::time::Duration::from_secs(900),
        fbuild_build::get_orchestrator(Platform::Ci13xx)
            .unwrap()
            .build(&super::ci13xx_fastled_acceptance::params(project)),
    )
    .await
    .expect("native configuration build timeout")
    .expect("real CI1306 Arduino build")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "#1914: actual Nuclei GCC/Chipintelli SDK compile/link of project-root Arduino sketch"]
async fn ci1306_root_ino_preprocessing_build() {
    let project = project(true, None, None);
    assert!(!project.path().join("src").exists());
    let result = build(project.path()).await;
    assert!(result.success);
    let records: Vec<serde_json::Value> =
        serde_json::from_slice(&std::fs::read(result.compile_database_path.unwrap()).unwrap())
            .unwrap();
    assert!(
        records
            .iter()
            .any(|r| r["file"].as_str().unwrap_or("").ends_with("main.ino.cpp")),
        "project-root sketch must actually compile via shared Arduino preprocessing"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "#1914: actual Nuclei GCC/Chipintelli SDK compile/link with selected board memory limits"]
async fn ci1306_uses_selected_board_memory_limits() {
    let project = project(false, None, Some((377777, 511111)));
    let result = build(project.path()).await;
    assert!(result.success);
    let size = result.size_info.expect("real target size output");
    assert!(size.total_flash > 0);
    assert_eq!(
        size.max_flash,
        Some(377777),
        "selected board flash budget must survive actual build"
    );
    assert_eq!(
        size.max_ram,
        Some(511111),
        "selected board RAM budget must survive actual build"
    );
}
