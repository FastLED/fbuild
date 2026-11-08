//! Real Arduino Due (SAM3X8E) linkage regression for FastLED/fbuild#1939.
//!
//! The classic-SAM link used to report success while producing an image with
//! no sketch in it: Atmel's `flash.ld` declares no `ENTRY()`, so nothing rooted
//! the reset handler, `libsam`'s startup object was never extracted from the
//! archive, and `--gc-sections` discarded `setup`, `loop` and `main`. A
//! successful `fbuild build` therefore said nothing about whether the
//! application had been linked at all.

use fbuild_build::{BuildOrchestrator, BuildParams, compile_backend};
use fbuild_core::BuildProfile;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "#1939: downloads ARM GCC and the Arduino SAM core"]
async fn sam3x_due_sketch_reaches_the_linked_image() {
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

    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path();
    std::fs::write(
        project.join("platformio.ini"),
        "[env:due]\nplatform = atmelsam\nboard = due\nframework = arduino\n",
    )
    .unwrap();
    std::fs::create_dir(project.join("src")).unwrap();
    std::fs::write(
        project.join("src/main.cpp"),
        r#"
#include <Arduino.h>

// Present so a discarded sketch object is unmistakable in the symbol table.
__attribute__((used)) int due_link_probe_marker = 7;

void setup() {
    pinMode(LED_BUILTIN, OUTPUT);
    Serial.begin(115200);
}

void loop() {
    digitalWrite(LED_BUILTIN, HIGH);
    delay(500);
}
"#,
    )
    .unwrap();

    let params = BuildParams {
        project_dir: project.to_path_buf(),
        shrink: Default::default(),
        env_name: "due".into(),
        build_dir: project
            .join(fbuild_paths::FBUILD_DIR_NAME)
            .join(fbuild_paths::BUILD_DIR_NAME)
            .join("due/release"),
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
    .expect("SAM3X build timed out")
    .unwrap_or_else(|error| panic!("due: {error}"));
    assert!(result.success, "due");
    let elf_path = result.elf_path.as_ref().unwrap();
    assert!(elf_path.is_file(), "due");

    let elf = fbuild_test_support::ElfProbe::open(elf_path).unwrap();
    let symbols = elf.symbols().unwrap();
    for required in ["Reset_Handler", "main", "setup", "loop"] {
        let symbol = symbols
            .iter()
            .find(|symbol| symbol.name == required && !symbol.is_undefined)
            .unwrap_or_else(|| {
                panic!("due: firmware.elf must define `{required}`; a build success is not a link")
            });
        assert!(
            symbol.size > 0,
            "due: `{required}` must occupy code, not be a discarded stub"
        );
    }
    assert!(!symbols.is_empty());
    assert!(result.size_info.as_ref().unwrap().total_flash > 0, "due");
}
