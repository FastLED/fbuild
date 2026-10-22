//! Real default Raspberry Pi Pico build regression for FastLED/fbuild#1670.

use fbuild_build::{BuildOrchestrator, BuildParams, compile_backend};
use fbuild_core::BuildProfile;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "#1670: downloads Arduino-Pico, matching pqt-gcc and published USB profiles"]
async fn pico_default_build_assembles_boot2_and_preserves_string_defines() {
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
        "[env:pico]\nplatform = raspberrypi\nboard = pico\nframework = arduino\n",
    )
    .unwrap();
    std::fs::create_dir(project.join("src")).unwrap();
    std::fs::write(project.join("src/main.cpp"), r#"
#include <Arduino.h>
#include <string_view>
constexpr char manufacturer[] = USB_MANUFACTURER;
constexpr char product[] = USB_PRODUCT;
static_assert(std::string_view(manufacturer) == "Raspberry Pi" &&
              std::string_view(product) == "Pico", "Pico USB strings survive compiler arguments");
void setup() { pinMode(LED_BUILTIN, OUTPUT); }
void loop() { digitalWrite(LED_BUILTIN, HIGH); }
"#).unwrap();
    let build_dir = project
        .join(fbuild_paths::FBUILD_DIR_NAME)
        .join(fbuild_paths::BUILD_DIR_NAME)
        .join("pico/release");
    let params = BuildParams {
        project_dir: project.to_path_buf(),
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
}
