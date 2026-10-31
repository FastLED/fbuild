//! Real GNU link failure regression for #1658, using the production linker.

use fbuild_build::generic_arm::{ArmLinker, ArmMcuConfig};
use fbuild_build::linker::{LinkExtraArgs, Linker};
use fbuild_core::{BuildProfile, subprocess::run_command};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "#1658: downloads ESP32-S3 toolchain/framework and published USB profiles"]
async fn esp32s3_dram_overflow_still_fails_with_region_summary() {
    use fbuild_build::{BuildOrchestrator, BuildParams, compile_backend};
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
    let root = tmp.path();
    std::fs::write(
        root.join("platformio.ini"),
        "[env:overflow]\nplatform = espressif32\nboard = esp32-s3-devkitc-1\nframework = arduino\n",
    )
    .unwrap();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.cpp"),
        "#include <Arduino.h>\nvolatile unsigned char oversized[400 * 1024];\nvoid setup() { oversized[0] = 1; }\nvoid loop() { oversized[sizeof(oversized) - 1] = oversized[0]; }\n").unwrap();
    let params = BuildParams {
        project_dir: root.into(),
        shrink: Default::default(),
        env_name: "overflow".into(),
        clean_all: false,
        clean_only: false,
        clean: true,
        profile: BuildProfile::Release,
        build_dir: root.join("build"),
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
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(900),
        fbuild_build::esp32::orchestrator::Esp32Orchestrator.build(&params),
    )
    .await
    .expect("ESP32-S3 regression exceeded 15 minute budget")
    .err()
    .expect("400KB BSS must fail to link")
    .to_string();
    assert!(error.contains("RAM: dram0_0_seg overflowed by"), "{error}");
    assert!(
        error.contains("(region ") && !error.contains("region size unavailable"),
        "{error}"
    );
    assert!(
        error.contains("requires ") && error.contains('%'),
        "{error}"
    );
    assert!(!params.build_dir.join("firmware.elf").exists());
    assert!(!params.build_dir.join("firmware.bin").exists());
    println!("{error}");
}

#[tokio::test]
#[ignore = "#1658: requires native GCC and GNU ld (Linux integration lane)"]
async fn failed_link_reports_each_actual_region_capacity() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(root.join("main.c"),
        "volatile char data[512]; const char table[512] = {1}; void _start(void) { data[0] = table[0]; }").unwrap();
    std::fs::write(
        root.join("memory.ld"),
        r#"
MEMORY {
    FLASH (rx) : ORIGIN = 0x10000, LENGTH = 256
    RAM (rw) : ORIGIN = 0x20000, LENGTH = 256
}
SECTIONS {
    .text : { *(.text*) *(.rodata*) } > FLASH
    .data : { *(.data*) } > RAM
    .bss : { *(.bss*) *(COMMON) } > RAM
}
"#,
    )
    .unwrap();
    let compile = run_command(
        &[
            "gcc",
            "-c",
            "-ffreestanding",
            "-fno-pie",
            "-fno-asynchronous-unwind-tables",
            "main.c",
            "-o",
            "main.o",
        ],
        Some(root),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(compile.success(), "{}", compile.stderr);
    let config: ArmMcuConfig = serde_json::from_value(serde_json::json!({
        "name":"native GNU regression", "architecture":"native",
        "compiler_flags":{"common":[],"c":[],"cxx":[]},
        "linker_flags":["-nostdlib","-no-pie","-Wl,--build-id=none"],
        "linker_libs":[], "profiles":{},
        "objcopy":{"output_format":"binary","remove_sections":[]}
    }))
    .unwrap();
    let linker = ArmLinker::new(
        "gcc".into(),
        "ar".into(),
        "objcopy".into(),
        "size".into(),
        root.join("memory.ld"),
        config,
        BuildProfile::Release,
        Some(1024 * 1024),
        Some(1024 * 1024),
        false,
    );
    let error = linker
        .link_all(
            &[root.join("main.o")],
            &[],
            root,
            &LinkExtraArgs::default(),
            false,
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("RAM: RAM overflowed by 256 bytes (region 256 bytes)"),
        "{error}"
    );
    assert!(error.contains("Flash: FLASH overflowed by"), "{error}");
    assert!(!root.join("firmware.elf").exists());
    assert!(!root.join("firmware.bin").exists());
}
