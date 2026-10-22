//! Real SAMD core/interrupt linkage regression for FastLED/fbuild#1672.

use fbuild_build::{BuildOrchestrator, BuildParams, compile_backend};
use fbuild_core::BuildProfile;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "#1672: downloads ARM GCC, SAMD core, CMSIS and published USB profiles"]
async fn samd_sketch_architecture_and_interrupt_override() {
    // Match daemon startup: compile identities come from the verified registry,
    // never from literals or synthetic IDs in this real-firmware test.
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
    for (board, flags) in [
        ("adafruit_feather_m0", ""),
        ("adafruit_feather_m4", "-flto"),
    ] {
        eprintln!("Building {board} with a sketch-provided EIC handler");
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path();
        std::fs::write(
            project.join("platformio.ini"),
            format!("[env:samd]\nplatform = atmelsam\nboard = {board}\nframework = arduino\nbuild_flags = {flags}\n"),
        )
        .unwrap();
        std::fs::create_dir(project.join("src")).unwrap();
        std::fs::write(
            project.join("src/main.cpp"),
            r#"
#include <Arduino.h>
#if !defined(ARDUINO_ARCH_SAMD) || defined(ARDUINO_ARCH_SAM)
#error incorrect SAMD architecture selection
#endif
extern "C" void EIC_Handler(void) {}
void setup() { pinMode(LED_BUILTIN, OUTPUT); }
void loop() { digitalWrite(LED_BUILTIN, HIGH); }
"#,
        )
        .unwrap();
        let params = BuildParams {
            project_dir: project.to_path_buf(),
            env_name: "samd".into(),
            build_dir: project
                .join(fbuild_paths::FBUILD_DIR_NAME)
                .join(fbuild_paths::BUILD_DIR_NAME)
                .join("samd/release"),
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
            fbuild_build::sam::SamOrchestrator.build(&params),
        )
        .await
        .expect("SAMD build timed out")
        .unwrap_or_else(|error| panic!("{board}: {error}"));
        assert!(result.success, "{board}");
        assert!(result.elf_path.as_ref().unwrap().is_file(), "{board}");
        let elf = fbuild_test_support::ElfProbe::open(result.elf_path.as_ref().unwrap()).unwrap();
        assert!(
            elf.symbols()
                .unwrap()
                .iter()
                .any(|symbol| { symbol.name == "Reset_Handler" && !symbol.is_undefined }),
            "{board}: firmware must retain its real reset entrypoint"
        );
        assert!(
            result.size_info.as_ref().unwrap().total_flash > 0,
            "{board}"
        );
    }
}
