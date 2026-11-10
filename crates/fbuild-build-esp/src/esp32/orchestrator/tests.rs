//! Unit tests for the ESP32 orchestrator's helpers and public API.

use super::super::espidf_delegate::{BUILD_SYSTEM_KEY, EspBuildSystem, IdfCacheMode};
use super::Esp32Orchestrator;
use super::cdc::{cdc_on_boot_enabled, is_esp32_project, warn_if_cdc_on_boot};
use super::helpers::{
    Esp32Integration, IdfCmakeProof, apply_effective_define_flags, esp32_idf_cmake_stack_summary,
    esp32_package_stack_summary, reject_unsupported_esp_framework,
    reject_unsupported_sdkconfig_overlay, requested_frameworks,
};
use super::helpers::{
    framework_failure_marker, framework_signature, record_failed_framework_lib,
    should_skip_failed_framework_lib,
};
use crate::BuildOrchestrator;
use fbuild_core::Platform;
use fbuild_core::path::NormalizedPath;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

#[test]
fn test_esp32_orchestrator_platform() {
    let orch = Esp32Orchestrator;
    assert_eq!(orch.platform(), Platform::Espressif32);
}

#[test]
fn sdkconfig_overlays_fail_for_arduino_esp32() {
    for key in ["board_build.sdkconfig_defaults", "custom_sdkconfig"] {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            format!(
                "[env]\nframework = arduino\n{key} = tools/size.defaults\n\
                 [env:esp32s3]\nplatform = espressif32\nboard = esp32-s3-devkitc-1\n"
            ),
        )
        .unwrap();
        let config =
            fbuild_config::PlatformIOConfig::from_path(&tmp.path().join("platformio.ini")).unwrap();
        let error = reject_unsupported_sdkconfig_overlay(&config, "esp32s3").unwrap_err();
        assert!(error.to_string().contains(key), "{error}");
        assert!(error.to_string().contains("#1460"), "{error}");
    }
}

fn env_with_framework(framework: &str) -> (tempfile::TempDir, fbuild_config::PlatformIOConfig) {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("platformio.ini"),
        format!(
            "[env:esp32dev]\nplatform = espressif32\nboard = esp32dev\nframework = {framework}\n"
        ),
    )
    .unwrap();
    let config =
        fbuild_config::PlatformIOConfig::from_path(&tmp.path().join("platformio.ini")).unwrap();
    (tmp, config)
}

#[test]
fn bare_espidf_framework_is_rejected_before_building() {
    let (_tmp, config) = env_with_framework("espidf");
    let error =
        reject_unsupported_esp_framework(&config, "esp32dev", EspBuildSystem::Native).unwrap_err();
    assert!(error.to_string().contains("#1664"), "{error}");
    assert!(error.to_string().contains(BUILD_SYSTEM_KEY), "{error}");
}

/// The delegated-build opt-in is what permits a bare `framework = espidf`
/// project; without it the #1730 rejection is unchanged (FastLED/fbuild#1971
/// AC2).
#[test]
fn the_delegated_opt_in_permits_a_bare_espidf_project() {
    let (_tmp, config) = env_with_framework("espidf");
    reject_unsupported_esp_framework(&config, "esp32dev", EspBuildSystem::EspIdf)
        .expect("the opt-in must permit the delegated build");
}

#[test]
fn arduino_and_dual_frameworks_are_accepted() {
    for framework in ["arduino", "arduino, espidf", "espidf, arduino"] {
        let (_tmp, config) = env_with_framework(framework);
        reject_unsupported_esp_framework(&config, "esp32dev", EspBuildSystem::Native)
            .unwrap_or_else(|error| panic!("{framework}: {error}"));
    }
}

#[test]
fn sdkconfig_guard_allows_plain_arduino_build() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("platformio.ini"),
        "[env:esp32s3]\nframework = arduino\nboard = esp32-s3-devkitc-1\n",
    )
    .unwrap();
    let config =
        fbuild_config::PlatformIOConfig::from_path(&tmp.path().join("platformio.ini")).unwrap();
    reject_unsupported_sdkconfig_overlay(&config, "esp32s3").unwrap();
}

#[tokio::test]
async fn sdkconfig_overlay_fails_before_cleaning_build_directory() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("platformio.ini"),
        "[env:esp32s3]\nplatform = espressif32\nboard = esp32-s3-devkitc-1\nframework = arduino\ncustom_sdkconfig = sdkconfig.defaults\n",
    )
    .unwrap();
    let build_dir = tmp.path().join("build");
    std::fs::create_dir(&build_dir).unwrap();
    let marker = build_dir.join("keep.txt");
    std::fs::write(&marker, "keep").unwrap();
    let params = crate::BuildParams {
        project_dir: tmp.path().to_path_buf(),
        shrink: Default::default(),
        env_name: "esp32s3".into(),
        clean_all: false,
        clean_only: false,
        clean: true,
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
    };
    let error = Esp32Orchestrator.build(&params).await.err().unwrap();
    assert!(error.to_string().contains("custom_sdkconfig"), "{error}");
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "keep");
}

#[test]
fn test_is_esp32_project() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("platformio.ini"),
        "[env:esp32c6]\nplatform = espressif32\nboard = esp32-c6\nframework = arduino\n",
    )
    .unwrap();
    assert!(is_esp32_project(tmp.path(), "esp32c6"));
    assert!(!is_esp32_project(tmp.path(), "uno"));
}

#[test]
fn test_is_not_esp32_project() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("platformio.ini"),
        "[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n",
    )
    .unwrap();
    assert!(!is_esp32_project(tmp.path(), "uno"));
}

// --- CDC on boot warning tests ---

/// Board that enables CDC on boot via extra_flags (e.g. Adafruit Feather ESP32-S3).
#[test]
fn test_cdc_enabled_by_board_extra_flags() {
    let board_flags = Some(
        "-DARDUINO_ADAFRUIT_FEATHER_ESP32S3 -DARDUINO_USB_CDC_ON_BOOT=1 -DARDUINO_RUNNING_CORE=1",
    );
    assert!(cdc_on_boot_enabled(board_flags, &[]));
}

/// Board that explicitly disables CDC on boot.
#[test]
fn test_cdc_disabled_by_board_extra_flags() {
    let board_flags = Some("-DARDUINO_FREENOVE_ESP32_S3_WROOM -DARDUINO_USB_CDC_ON_BOOT=0");
    assert!(!cdc_on_boot_enabled(board_flags, &[]));
}

/// Plain ESP32 dev board with no CDC flag at all — not enabled.
#[test]
fn test_no_cdc_flag_returns_false() {
    let board_flags = Some("-DARDUINO_ESP32_DEV");
    assert!(!cdc_on_boot_enabled(board_flags, &[]));
}

/// No board flags at all — not enabled.
#[test]
fn test_no_flags_at_all_returns_false() {
    assert!(!cdc_on_boot_enabled(None, &[]));
}

/// User build_flags override a board-level enable (last definition wins).
#[test]
fn test_user_flag_overrides_board_enable() {
    let board_flags = Some("-DARDUINO_USB_CDC_ON_BOOT=1");
    let user_flags = vec!["-DARDUINO_USB_CDC_ON_BOOT=0".to_string()];
    assert!(!cdc_on_boot_enabled(board_flags, &user_flags));
}

/// User build_flags can enable CDC that the board left unconfigured.
#[test]
fn test_user_flag_enables_cdc() {
    let board_flags = Some("-DARDUINO_ESP32_DEV");
    let user_flags = vec!["-DARDUINO_USB_CDC_ON_BOOT=1".to_string()];
    assert!(cdc_on_boot_enabled(board_flags, &user_flags));
}

/// Multiple user flags — last one wins.
#[test]
fn test_last_user_flag_wins() {
    let board_flags = Some("-DARDUINO_USB_CDC_ON_BOOT=1");
    let user_flags = vec![
        "-DARDUINO_USB_CDC_ON_BOOT=0".to_string(),
        "-DARDUINO_USB_CDC_ON_BOOT=1".to_string(),
    ];
    assert!(cdc_on_boot_enabled(board_flags, &user_flags));
}

/// Flags provided as whitespace-separated string should be parsed correctly.
#[test]
fn test_multi_flag_string_parsed_correctly() {
    // Board flags: the enable flag appears after another flag.
    let board_flags = Some("-DSOME_DEFINE -DARDUINO_USB_CDC_ON_BOOT=1 -DANOTHER=1");
    assert!(cdc_on_boot_enabled(board_flags, &[]));
}

/// `warn_if_cdc_on_boot` should not panic for any combination of inputs.
#[test]
fn test_warn_if_cdc_on_boot_no_panic() {
    // CDC enabled — triggers warning path
    warn_if_cdc_on_boot(
        "Adafruit Feather ESP32-S3",
        Some("-DARDUINO_USB_CDC_ON_BOOT=1"),
        &[],
    );
    // CDC disabled — no warning
    warn_if_cdc_on_boot(
        "Freenove ESP32-S3-WROOM",
        Some("-DARDUINO_USB_CDC_ON_BOOT=0"),
        &[],
    );
    // No flag at all — no warning
    warn_if_cdc_on_boot("ESP32 Dev Module", Some("-DARDUINO_ESP32_DEV"), &[]);
    // No board flags — no warning
    warn_if_cdc_on_boot("Some Board", None, &[]);
    // User override suppresses board enable
    warn_if_cdc_on_boot(
        "Some Board",
        Some("-DARDUINO_USB_CDC_ON_BOOT=1"),
        &["-DARDUINO_USB_CDC_ON_BOOT=0".to_string()],
    );
}

#[test]
fn test_framework_signature_changes_with_flags() {
    let includes = vec![PathBuf::from("C:/sdk/include")];
    let sig_a = framework_signature(
        &includes,
        &["-O2".to_string()],
        &["-std=gnu++17".to_string()],
    );
    let sig_b = framework_signature(
        &includes,
        &["-O0".to_string()],
        &["-std=gnu++17".to_string()],
    );
    assert_ne!(sig_a, sig_b);
}

#[test]
fn test_skip_failed_framework_lib_when_marker_matches_and_is_current() {
    let tmp = tempfile::TempDir::new().unwrap();
    let source = tmp.path().join("Matter.cpp");
    std::fs::write(&source, "int x;").unwrap();
    let marker = framework_failure_marker(tmp.path(), "matter");
    let sig = framework_signature(&[], &["-O2".to_string()], &["-std=gnu++2b".to_string()]);
    std::thread::sleep(Duration::from_millis(20));
    record_failed_framework_lib(&marker, &sig, "compile failed");

    assert!(should_skip_failed_framework_lib(&marker, &sig, &[source]).unwrap());
}

#[test]
fn test_retry_failed_framework_lib_after_source_change() {
    let tmp = tempfile::TempDir::new().unwrap();
    let source = tmp.path().join("Matter.cpp");
    std::fs::write(&source, "int x;").unwrap();
    let marker = framework_failure_marker(tmp.path(), "matter");
    let sig = framework_signature(&[], &["-O2".to_string()], &["-std=gnu++2b".to_string()]);
    std::thread::sleep(Duration::from_millis(20));
    record_failed_framework_lib(&marker, &sig, "compile failed");
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(&source, "int y;").unwrap();

    assert!(!should_skip_failed_framework_lib(&marker, &sig, &[source]).unwrap());
}

#[test]
fn test_retry_failed_framework_lib_after_signature_change() {
    let tmp = tempfile::TempDir::new().unwrap();
    let source = tmp.path().join("Matter.cpp");
    std::fs::write(&source, "int x;").unwrap();
    let marker = framework_failure_marker(tmp.path(), "matter");
    let sig_a = framework_signature(&[], &["-O2".to_string()], &["-std=gnu++2b".to_string()]);
    let sig_b = framework_signature(&[], &["-O0".to_string()], &["-std=gnu++2b".to_string()]);
    std::thread::sleep(Duration::from_millis(20));
    record_failed_framework_lib(&marker, &sig_a, "compile failed");

    assert!(!should_skip_failed_framework_lib(&marker, &sig_b, &[source]).unwrap());
}

#[test]
fn effective_define_flags_match_compiler_overlay_order() {
    let mut defines = std::collections::HashMap::from([
        ("BOARD_ONLY".to_string(), "1".to_string()),
        ("DISABLED_BY_UNFLAG".to_string(), "1".to_string()),
    ]);
    let sdk_flags = vec![
        "-DDISABLED_BY_UNFLAG".to_string(),
        "-DENABLE_WIFI".to_string(),
    ];
    let user_flags = vec![
        "-DDISABLED_BY_UNFLAG=0".to_string(),
        "-DREMOVED_USER_DEFINE".to_string(),
        "-DVALUE=42".to_string(),
        "-UVALUE".to_string(),
    ];
    let build_unflags = vec![
        "-DDISABLED_BY_UNFLAG".to_string(),
        "-DREMOVED_USER_DEFINE".to_string(),
    ];

    apply_effective_define_flags(&mut defines, &sdk_flags, &user_flags, &build_unflags);

    assert_eq!(defines.get("BOARD_ONLY"), Some(&"1".to_string()));
    assert_eq!(defines.get("ENABLE_WIFI"), Some(&"1".to_string()));
    assert_eq!(
        defines.get("DISABLED_BY_UNFLAG"),
        Some(&"0".to_string()),
        "user flags must override an unflagged SDK definition"
    );
    assert!(
        !defines.contains_key("REMOVED_USER_DEFINE"),
        "an exact user flag listed in build_unflags must remain removed"
    );
    assert!(!defines.contains_key("VALUE"));
}

/// FastLED/fbuild#1616: deploy reuses the build's esptool only when it speaks
/// the v5 hyphenated CLI the deployer emits; the v4 `esptool.py` source
/// package falls back to an `esptool` on PATH.
#[test]
fn deploy_esptool_requires_v5_cli() {
    use super::packages::speaks_v5_cli;
    use std::path::Path;
    assert!(speaks_v5_cli(Path::new(
        "/cache/tasmota-esptool/5.1.0/esptool-linux-amd64/esptool"
    )));
    assert!(speaks_v5_cli(Path::new("C:/cache/esptool/esptool.exe")));
    assert!(!speaks_v5_cli(Path::new(
        "/cache/tool-esptoolpy/fbuild-tools/esptool/bin/esptool.py"
    )));
    assert!(!speaks_v5_cli(Path::new(
        "C:/cache/fbuild-tools/esptool/Scripts/esptool.py.exe"
    )));
}

fn package_info(name: &str, version: &str) -> fbuild_packages::PackageInfo {
    fbuild_packages::PackageInfo {
        name: name.to_string(),
        version: version.to_string(),
        url: format!("https://example.invalid/{name}-{version}.tar.gz"),
        install_path: PathBuf::from("/nonexistent"),
        checksum: None,
        installed_bytes: None,
    }
}

#[test]
fn requested_frameworks_parses_single_and_dual_requests() {
    let arduino = requested_frameworks("arduino");
    assert!(arduino.arduino && !arduino.espidf);
    assert_eq!(arduino.integration(), Some(Esp32Integration::ArduinoCore));

    let dual = requested_frameworks("arduino, espidf");
    assert!(dual.arduino && dual.espidf);
    assert_eq!(dual.integration(), Some(Esp32Integration::ArduinoOnIdf));

    // Order and spacing must not change the classification.
    let reversed = requested_frameworks(" espidf , arduino ");
    assert_eq!(reversed, dual);

    let bare_idf = requested_frameworks("espidf");
    assert_eq!(bare_idf.integration(), None, "bare espidf is unsupported");
}

#[test]
fn stack_summary_records_requested_framework_and_integration_mode() {
    let tmp = tempfile::TempDir::new().unwrap();
    let installed = fbuild_packages::library::Esp32Framework::new(tmp.path(), "esp32s3");
    let platform = package_info("espressif32", "6.13.0");
    let framework = package_info("framework-arduinoespressif32", "3.3.7");
    let toolchain = package_info("toolchain-xtensa-esp32s3", "8.4.0");

    let mut dual_env = HashMap::new();
    dual_env.insert("framework".to_string(), "arduino, espidf".to_string());
    dual_env.insert(
        "platform_packages".to_string(),
        "framework-arduinoespressif32@https://example.invalid/fw.tar.gz".to_string(),
    );
    let summary = esp32_package_stack_summary(
        Some(&dual_env),
        &platform,
        &framework,
        &toolchain,
        &installed,
        "esp32s3",
    );
    assert!(summary.contains("framework=arduino, espidf"), "{summary}");
    assert!(summary.contains("integration=arduino-on-idf"), "{summary}");
    assert!(
        summary.contains(
            "framework_pin=framework-arduinoespressif32@https://example.invalid/fw.tar.gz"
        ),
        "{summary}"
    );
    assert!(
        summary.contains(
            "resolved platform=espressif32@6.13.0, framework=framework-arduinoespressif32@3.3.7"
        ),
        "{summary}"
    );

    let plain = esp32_package_stack_summary(
        None, &platform, &framework, &toolchain, &installed, "esp32s3",
    );
    assert!(plain.contains("framework=<board default>"), "{plain}");
    assert!(
        plain.contains("framework_pin=<platform manifest>"),
        "{plain}"
    );
    assert!(plain.contains("integration=arduino-core"), "{plain}");
}

/// A set of published delegated artifacts in a throwaway root, so the proof
/// gate can be exercised against real files instead of fabricated counts.
fn artifact_fixture(
    app_bin: &[u8],
    elf: &[u8],
    compile_db: &str,
) -> (
    tempfile::TempDir,
    super::super::espidf_artifacts::IdfArtifacts,
) {
    let root = tempfile::TempDir::new().unwrap();
    let dir = NormalizedPath::new(root.path());
    let write = |name: &str, bytes: &[u8]| {
        let path = dir.join(name);
        std::fs::write(path.as_path(), bytes).unwrap();
        path
    };
    let artifacts = super::super::espidf_artifacts::IdfArtifacts {
        project: "hello_world".to_string(),
        app_bin: write("hello_world.bin", app_bin),
        elf: write("hello_world.elf", elf),
        map: write("hello_world.map", b"map"),
        bootloader_bin: write("bootloader.bin", b"boot"),
        partition_table_bin: write("partition-table.bin", b"pt"),
        sdkconfig: write("sdkconfig", b"CONFIG_IDF_TARGET=\"esp32s3\"\n"),
        compile_commands: write("compile_commands.json", compile_db.as_bytes()),
    };
    (root, artifacts)
}

const ONE_ENTRY_DB: &str =
    r#"[{"directory":"/proj/build","file":"/proj/main/app_main.c","command":"cc app_main.c"}]"#;

/// The gate is the constructor: `integration=idf-cmake` cannot be produced
/// from a build that did not leave an app image, an ELF and a compile database
/// entry behind (FastLED/fbuild#1972).
#[test]
fn idf_cmake_proof_is_refused_without_artifacts() {
    let valid_db = ONE_ENTRY_DB;
    let (_root, complete) = artifact_fixture(b"\x7fELFbin", b"\x7fELFelf", valid_db);
    let proof = IdfCmakeProof::verified(&complete, "v5.5.1").expect("complete artifacts prove it");
    assert!(proof.app_bin_bytes() > 0, "app image bytes are recorded");
    assert!(proof.elf_bytes() > 0, "ELF bytes are recorded");
    assert_eq!(proof.compile_db_entries(), 1);
    assert_eq!(proof.sdkconfig_sha256().len(), 64, "sdkconfig is digested");

    // Each proof element on its own is enough to refuse the claim.
    let (_root, empty_bin) = artifact_fixture(b"", b"\x7fELFelf", valid_db);
    let refused =
        IdfCmakeProof::verified(&empty_bin, "v5.5.1").expect_err("an empty app image is not proof");
    assert!(refused.to_string().contains("empty"), "{refused}");

    let (_root, empty_elf) = artifact_fixture(b"\x7fELFbin", b"", valid_db);
    assert!(IdfCmakeProof::verified(&empty_elf, "v5.5.1").is_err());

    let (_root, empty_db) = artifact_fixture(b"\x7fELFbin", b"\x7fELFelf", "[]");
    let refused = IdfCmakeProof::verified(&empty_db, "v5.5.1")
        .expect_err("zero compile commands is not proof");
    assert!(
        refused.to_string().contains("empty compile database"),
        "{refused}"
    );
}

/// The claimed mode and the pinned release are on the summary line, and the
/// proof numbers travel with it so a log can be checked against the artifacts.
#[test]
fn idf_cmake_summary_names_the_pinned_idf_and_proof() {
    let (_root, complete) = artifact_fixture(b"\x7fELFbin", b"\x7fELFelf", ONE_ENTRY_DB);
    let proof = IdfCmakeProof::verified(&complete, "v5.5.1").unwrap();
    let summary = esp32_idf_cmake_stack_summary(&proof, "esp32s3", IdfCacheMode::ColdOnly);
    assert!(summary.contains("integration=idf-cmake"), "{summary}");
    assert!(summary.contains("idf=v5.5.1"), "{summary}");
    assert!(summary.contains("chip=esp32s3"), "{summary}");
    assert!(summary.contains("compile_db_entries=1"), "{summary}");
    assert!(summary.contains("sdkconfig_sha256="), "{summary}");
}

/// The summary states the caching mode the build actually ran in, and the
/// delegated path has none: it must say `cold-only` and must never read as if
/// objects came out of a cache (FastLED/fbuild#1974).
#[test]
fn idf_cmake_summary_claims_cold_only_caching_and_no_cache_hit() {
    let (_root, complete) = artifact_fixture(b"\x7fELFbin", b"\x7fELFelf", ONE_ENTRY_DB);
    let proof = IdfCmakeProof::verified(&complete, "v5.5.1").unwrap();
    let summary = esp32_idf_cmake_stack_summary(&proof, "esp32s3", IdfCacheMode::ColdOnly);
    assert!(summary.contains("cache=cold-only"), "{summary}");
    assert!(
        !summary.to_ascii_lowercase().contains("cached"),
        "a delegated build serves nothing from a cache: {summary}"
    );
    assert!(
        !summary.contains("cache=hit"),
        "an unwrapped build cannot claim a cache hit: {summary}"
    );
}

/// The requested-framework path inspects `framework =` only, so it has no
/// build result to prove anything with and must never name `idf-cmake`.
#[test]
fn requested_frameworks_can_never_claim_idf_cmake() {
    for raw in [
        "arduino",
        "espidf",
        "arduino, espidf",
        "",
        "ESPIDF",
        "espidf, espidf",
    ] {
        let integration = requested_frameworks(raw).integration();
        assert!(
            !matches!(integration, Some(Esp32Integration::IdfCmake(_))),
            "framework = {raw} must not name idf-cmake"
        );
    }
}
