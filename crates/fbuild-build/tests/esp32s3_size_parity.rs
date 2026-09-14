//! Size-parity smoke test for FastLED/fbuild#1432: an ESP32-S3 Blink built by
//! fbuild must not produce a larger flash image than PlatformIO builds from the
//! same `platformio.ini`.
//!
//! Both builds pin the same pioarduino platform release, so the comparison
//! measures fbuild's build pipeline rather than a framework version change.
//!
//! `#[ignore]`-marked because it needs PlatformIO (`pio` on PATH, or the path
//! in `FBUILD_PARITY_PIO`) and downloads the ESP32 toolchain and framework for
//! both tools. `.github/workflows/esp32s3-size-parity.yml` runs it once a day:
//!
//! ```text
//! soldr cargo test -p fbuild-build --test esp32s3_size_parity -- --ignored --nocapture
//! ```

use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::process::Command;

use fbuild_build::{BuildOrchestrator, BuildParams, compile_backend};
use fbuild_core::BuildProfile;
use fbuild_core::path::NormalizedPath;

const ENV_NAME: &str = "esp32s3";

/// The pioarduino release both builds pin (FastLED's `generic-esp` pin).
const PLATFORM_URL: &str = "https://github.com/pioarduino/platform-espressif32/releases/download/55.03.35/platform-espressif32.zip";

/// 15-min wall-clock cap for `--ignored` real-toolchain tests (FastLED/fbuild#806).
const REAL_BUILD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(900);

/// The orchestrator compiles through the process-wide compile backend
/// (FastLED/fbuild#800), which only the daemon wires at startup.
async fn install_test_compile_backend() {
    static INSTALL: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    INSTALL
        .get_or_init(|| async {
            let backend = compile_backend::CompileBackend::start()
                .await
                .expect("compile backend starts for size parity test");
            compile_backend::install_global(backend);
        })
        .await;
}

fn write_blink_project(project_dir: &Path) {
    fs::write(
        project_dir.join("platformio.ini"),
        format!(
            "[env:{ENV_NAME}]\nplatform = {PLATFORM_URL}\nboard = esp32-s3-devkitc-1\nframework = arduino\n"
        ),
    )
    .unwrap();

    let src_dir = project_dir.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(
        src_dir.join("main.cpp"),
        "\
#include <Arduino.h>

void setup() {
  pinMode(2, OUTPUT);
}

void loop() {
  digitalWrite(2, HIGH);
  delay(1000);
  digitalWrite(2, LOW);
  delay(1000);
}
",
    )
    .unwrap();
}

/// Build with PlatformIO and return the directory holding its artifacts.
fn build_with_platformio(project_dir: &Path) -> NormalizedPath {
    let pio = std::env::var_os("FBUILD_PARITY_PIO").unwrap_or_else(|| OsString::from("pio"));
    // allow-direct-spawn: integration test driver invoking the PlatformIO binary it compares against.
    let output = Command::new(&pio)
        .args(["run", "-e", ENV_NAME, "-d"])
        .arg(project_dir)
        .output()
        .unwrap_or_else(|e| panic!("failed to run PlatformIO ({pio:?}): {e}"));
    assert!(
        output.status.success(),
        "PlatformIO build failed ({}):\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    NormalizedPath::from(project_dir.join(".pio/build").join(ENV_NAME))
}

/// Build with fbuild's ESP32 orchestrator and return its artifact directory.
async fn build_with_fbuild(project_dir: &Path) -> NormalizedPath {
    let build_dir = project_dir.join(format!(
        "{}/{}/{ENV_NAME}/release",
        fbuild_paths::FBUILD_DIR_NAME,
        fbuild_paths::BUILD_DIR_NAME
    ));
    let params = BuildParams {
        project_dir: project_dir.to_path_buf(),
        env_name: ENV_NAME.to_string(),
        clean_all: false,
        clean_only: false,
        clean: true,
        profile: BuildProfile::Release,
        build_dir: build_dir.clone(),
        verbose: false,
        jobs: None,
        generate_compiledb: false,
        compiledb_only: false,
        log_sender: None,
        symbol_analysis: false,
        symbol_analysis_path: None,
        no_timestamp: false,
        src_dir: None,
        pio_env: Default::default(),
        extra_build_flags: Vec::new(),
        watch_set_cache: None,
        bloat_analysis: false,
        caller_path: None,
    };
    let orchestrator = fbuild_build::esp32::orchestrator::Esp32Orchestrator;
    let result = tokio::time::timeout(REAL_BUILD_TIMEOUT, orchestrator.build(&params))
        .await
        .expect("fbuild build exceeded the real-toolchain budget (FastLED/fbuild#806)")
        .expect("fbuild build should succeed");
    assert!(result.success, "fbuild build should report success");
    NormalizedPath::from(build_dir)
}

/// Non-debug ELF sections with their sizes, for a readable failure report.
fn section_sizes(elf: &Path) -> Vec<(String, u64)> {
    use object::{Object, ObjectSection};
    let bytes = fs::read(elf).unwrap_or_else(|e| panic!("read {}: {e}", elf.display()));
    let file = object::File::parse(&*bytes).expect("parse ELF");
    file.sections()
        .filter(|s| s.size() > 0)
        .filter_map(|s| {
            let name = s.name().ok()?;
            (!name.starts_with(".debug") && !name.is_empty()).then(|| (name.to_string(), s.size()))
        })
        .collect()
}

fn report(label: &str, artifacts: &Path) -> u64 {
    let bin = artifacts.join("firmware.bin");
    let size = fs::metadata(&bin)
        .unwrap_or_else(|e| panic!("{label} firmware.bin missing at {}: {e}", bin.display()))
        .len();
    println!("{label}: firmware.bin = {size} B");
    for (name, bytes) in section_sizes(&artifacts.join("firmware.elf")) {
        println!("  {name:<24} {bytes:>10}");
    }
    size
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires PlatformIO and downloads the ESP32 toolchain (~hundreds of MB) twice"]
async fn esp32s3_blink_is_no_larger_than_platformio() {
    install_test_compile_backend().await;
    // Separate project dirs so neither tool sees the other's build output.
    let pio_tmp = tempfile::TempDir::new().unwrap();
    let fbuild_tmp = tempfile::TempDir::new().unwrap();
    write_blink_project(pio_tmp.path());
    write_blink_project(fbuild_tmp.path());

    let pio_artifacts = build_with_platformio(pio_tmp.path());
    let fbuild_artifacts = build_with_fbuild(fbuild_tmp.path()).await;

    let pio_size = report("PlatformIO", &pio_artifacts);
    let fbuild_size = report("fbuild", &fbuild_artifacts);
    assert!(
        fbuild_size <= pio_size,
        "fbuild's ESP32-S3 Blink firmware.bin is {} B larger than PlatformIO's \
         (fbuild={fbuild_size} B, PlatformIO={pio_size} B); see the section tables above",
        fbuild_size - pio_size
    );
}
