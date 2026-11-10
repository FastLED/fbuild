//! Real Arduino-Pico and Arduino Mbed RP2040 build regressions (#1670, #1977).

use fbuild_build::{BuildOrchestrator, BuildParams, compile_backend};
use fbuild_core::BuildProfile;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "#1670/#1977: downloads both RP2040 cores, matching ARM toolchains and published USB profiles"]
async fn pico_core_families_link_with_their_own_sdk_and_boot_layout() {
    tokio::task::spawn_blocking(|| {
        let registry = tempfile::tempdir().unwrap();
        assert!(fbuild_core::usb::profiles::populate_profiles_from_paths(
            &registry.path().join("_meta.json"),
            &registry.path().join("usb-profiles.json"),
        ));
    })
    .await
    .unwrap();
    let backend = compile_backend::CompileBackend::start().await.unwrap();
    compile_backend::install_global(backend);
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path();
    std::fs::write(
        project.join("platformio.ini"),
        "[env:pico]\nplatform = raspberrypi\nboard = rpipico\nframework = arduino\nboard_build.core = earlephilhower\n",
    )
    .unwrap();
    std::fs::create_dir(project.join("src")).unwrap();
    std::fs::write(
        project.join("src/main.cpp"),
        r#"
#include <Arduino.h>
#include <string_view>
constexpr char manufacturer[] = USB_MANUFACTURER;
constexpr char product[] = USB_PRODUCT;
static_assert(std::string_view(manufacturer) == "Raspberry Pi" &&
              std::string_view(product) == "Pico", "Pico USB strings survive compiler arguments");
void setup() { pinMode(LED_BUILTIN, OUTPUT); }
void loop() { digitalWrite(LED_BUILTIN, HIGH); }
"#,
    )
    .unwrap();
    let build_dir = project
        .join(fbuild_paths::FBUILD_DIR_NAME)
        .join(fbuild_paths::BUILD_DIR_NAME)
        .join("pico/release");
    let params = BuildParams {
        project_dir: project.to_path_buf(),
        shrink: Default::default(),
        env_name: "pico".into(),
        build_dir: build_dir.clone(),
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
        fbuild_build::rp2040::Rp2040Orchestrator.build(&params),
    )
    .await
    .expect("Pico build timed out")
    .expect("Pico build must succeed");
    assert!(result.success);
    assert!(build_dir.join("boot2.o").is_file(), "boot2 assembly output");
    assert!(result.elf_path.as_ref().expect("firmware ELF").is_file());
    assert!(result.size_info.as_ref().unwrap().total_flash > 0);
    assert_mbed_build(params).await;
}

fn mbed_fixture() -> (tempfile::TempDir, tempfile::TempDir) {
    let project = tempfile::tempdir().unwrap();
    let dependency = tempfile::tempdir().unwrap();
    std::fs::create_dir(dependency.path().join("src")).unwrap();
    std::fs::write(
        dependency.path().join("library.json"),
        r#"{"name":"MbedFeatureGuard","version":"1.0.0"}"#,
    )
    .unwrap();
    std::fs::write(
        dependency.path().join("src/FeatureGuard.h"),
        "int mbed_library_value();\n",
    )
    .unwrap();
    std::fs::write(
        dependency.path().join("src/FeatureGuard.cpp"),
        r#"
#include "FeatureGuard.h"
#if !defined(MBED_LIBRARY_FLAG) || MBED_LIBRARY_FLAG != 42
#error Dependency lost the caller compile overlay
#endif
#ifdef PROJECT_ONLY
#error Source-only flags leaked into a foreign dependency
#endif
int mbed_library_value() { return MBED_LIBRARY_FLAG; }
"#,
    )
    .unwrap();
    std::fs::write(project.path().join("platformio.ini"), format!(
        "[env:mbed]\nplatform = raspberrypi\nboard = pico\nframework = arduino\nlib_deps = {}\nbuild_src_flags = -DPROJECT_ONLY=1\n", dependency.path().display())).unwrap();
    std::fs::create_dir(project.path().join("src")).unwrap();
    std::fs::write(
        project.path().join("src/main.cpp"),
        r#"
#include <Arduino.h>
#include <SPI.h>
#include <Wire.h>
#include <FeatureGuard.h>
#ifndef ARDUINO_ARCH_MBED_RP2040
#error Wrong core selected
#endif
#ifndef PROJECT_ONLY
#error Sketch lost source-only compile flags
#endif
volatile int feature_value;
void setup() { SPI.begin(); Wire.begin(); feature_value = mbed_library_value(); }
void loop() {}
"#,
    )
    .unwrap();
    (project, dependency)
}

async fn assert_mbed_build(template: BuildParams) {
    let (project, _dependency) = mbed_fixture();
    let params = BuildParams {
        project_dir: project.path().to_path_buf(),
        env_name: "mbed".into(),
        extra_build_flags: vec!["-DMBED_LIBRARY_FLAG=42".into()],
        build_dir: project.path().join(".fbuild/build/mbed/release"),
        ..template
    };
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(900),
        fbuild_build::rp2040::Rp2040Orchestrator.build(&params),
    )
    .await
    .expect("Mbed build timed out")
    .expect("stock pico Mbed build must succeed");
    assert!(result.success);
    let firmware = result.firmware_path.expect("Mbed firmware");
    assert_eq!(
        firmware.extension().and_then(|ext| ext.to_str()),
        Some("uf2")
    );
    assert!(firmware.is_file());
    assert!(result.elf_path.expect("Mbed ELF").is_file());
    assert!(result.size_info.unwrap().total_flash > 0);
    let clean = BuildParams {
        clean_only: true,
        ..params
    };
    let cleaned = fbuild_build::rp2040::Rp2040Orchestrator
        .build(&clean)
        .await
        .expect("clean-only must not attempt firmware conversion");
    assert!(cleaned.success);
    assert!(cleaned.elf_path.is_none());
    assert!(cleaned.firmware_path.is_none());
}
