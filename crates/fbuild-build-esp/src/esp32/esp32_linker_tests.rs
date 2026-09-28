use super::*;
use crate::esp32::mcu_config::get_mcu_config;

fn test_linker(mcu: &str) -> Esp32Linker {
    let config = get_mcu_config(mcu).unwrap();
    let prefix = config.toolchain_prefix();
    Esp32Linker::new(
        PathBuf::from(format!("/usr/bin/{}gcc", prefix)),
        PathBuf::from(format!("/usr/bin/{}ar", prefix)),
        PathBuf::from(format!("/usr/bin/{}objcopy", prefix)),
        PathBuf::from(format!("/usr/bin/{}size", prefix)),
        config,
        vec![
            "-nostartfiles".to_string(),
            "-u".to_string(),
            "app_main".to_string(),
        ],
        vec![
            "-L/sdk/lib".to_string(),
            "-lfreertos".to_string(),
            "-lesp_system".to_string(),
        ],
        LinkerScripts::from_raw_flags(&[
            "-L/sdk/ld".to_string(),
            "-Tmemory.ld".to_string(),
            "-Tsections.ld".to_string(),
        ]),
        BuildProfile::Release,
        None,
        "80m",
        Some(3145728),
        Some(327680),
        None,
        false,
    )
}

#[test]
fn test_esp32_linker_creation() {
    let linker = test_linker("esp32c6");
    assert_eq!(linker.max_flash, Some(3145728));
    assert_eq!(linker.max_ram, Some(327680));
}

#[test]
fn pinned_esptool_v4_uses_underscore_elf2image_options() {
    let v4 = esptool_elf2image_argv(
        Some(Path::new("/cache/bin/esptool.py")),
        "esp32s3",
        "dio",
        "80m",
        "8MB",
        "firmware.elf",
        "firmware.bin",
    );
    assert!(v4.iter().any(|arg| arg == "--flash_mode"));
    assert!(v4.iter().any(|arg| arg == "--flash_freq"));
    assert!(v4.iter().any(|arg| arg == "--flash_size"));
    assert!(!v4.iter().any(|arg| arg == "--flash-mode"));

    let v5 = esptool_elf2image_argv(
        Some(Path::new("/cache/bin/esptool")),
        "esp32s3",
        "dio",
        "80m",
        "8MB",
        "firmware.elf",
        "firmware.bin",
    );
    assert!(v5.iter().any(|arg| arg == "--flash-mode"));
}

fn test_linker_with(esptool_bin: Option<PathBuf>, caller_path: Option<String>) -> Esp32Linker {
    let mut linker = test_linker("esp32c6");
    linker.esptool_bin = esptool_bin;
    linker.caller_path = caller_path;
    linker
}

/// FastLED/fbuild#1238: with a bare-name esptool, two requests with
/// different caller PATHs may resolve different esptool binaries —
/// they must never share a cached firmware.bin.
#[test]
fn bare_name_esptool_bin_reuse_is_keyed_by_caller_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    let elf = tmp.path().join("firmware.elf");
    std::fs::write(&elf, b"elf").unwrap();

    let linker_a = test_linker_with(None, Some("C:\\venv-a\\Scripts".to_string()));
    let flash_size = linker_a.flash_size();

    // Simulate a successful conversion by linker A.
    std::fs::write(tmp.path().join("firmware.bin"), b"bin").unwrap();
    let cache = linker_a.current_bin_cache(&elf, &flash_size).unwrap();
    save_json(&linker_a.bin_cache_path(tmp.path()), &cache).unwrap();

    assert!(
        linker_a.can_reuse_bin(&elf, tmp.path(), &flash_size),
        "same caller PATH must reuse the cached bin"
    );

    let linker_b = test_linker_with(None, Some("C:\\venv-b\\Scripts".to_string()));
    assert!(
        !linker_b.can_reuse_bin(&elf, tmp.path(), &flash_size),
        "a different caller PATH must not reuse a bin produced by another PATH's esptool"
    );

    let linker_none = test_linker_with(None, None);
    assert!(
        !linker_none.can_reuse_bin(&elf, tmp.path(), &flash_size),
        "no caller PATH (daemon-ambient resolution) must not reuse a caller-PATH bin"
    );
}

/// Provisioned absolute-path esptool cannot drift with the caller's
/// PATH — caching must behave exactly as before, including across
/// requests with different caller PATHs.
#[test]
fn absolute_esptool_bin_reuse_ignores_caller_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    let elf = tmp.path().join("firmware.elf");
    std::fs::write(&elf, b"elf").unwrap();

    let esptool = PathBuf::from("C:\\tools\\esptool.exe");
    let linker_a = test_linker_with(Some(esptool.clone()), Some("C:\\venv-a".to_string()));
    let flash_size = linker_a.flash_size();

    std::fs::write(tmp.path().join("firmware.bin"), b"bin").unwrap();
    let cache = linker_a.current_bin_cache(&elf, &flash_size).unwrap();
    assert!(
        cache.esptool_fingerprint.is_empty(),
        "absolute esptool must record the serde-default (empty) fingerprint"
    );
    save_json(&linker_a.bin_cache_path(tmp.path()), &cache).unwrap();

    let linker_b = test_linker_with(Some(esptool), Some("C:\\venv-b".to_string()));
    assert!(
        linker_b.can_reuse_bin(&elf, tmp.path(), &flash_size),
        "absolute-path esptool must keep reusing regardless of caller PATH"
    );
}

#[test]
fn test_flash_size_uses_board_max_flash_for_elf2image_and_cache() {
    let config = get_mcu_config("esp32c6").unwrap();
    let prefix = config.toolchain_prefix();
    let linker = Esp32Linker::new(
        PathBuf::from(format!("/usr/bin/{}gcc", prefix)),
        PathBuf::from(format!("/usr/bin/{}ar", prefix)),
        PathBuf::from(format!("/usr/bin/{}objcopy", prefix)),
        PathBuf::from(format!("/usr/bin/{}size", prefix)),
        config,
        vec![],
        vec![],
        LinkerScripts::new(),
        BuildProfile::Release,
        None,
        "80m",
        Some(4 * 1024 * 1024),
        Some(327680),
        None,
        false,
    );
    let tmp = tempfile::TempDir::new().unwrap();
    let elf = tmp.path().join("firmware.elf");
    std::fs::write(&elf, b"elf").unwrap();

    let flash_size = linker.flash_size();
    let cache = linker.current_bin_cache(&elf, &flash_size).unwrap();

    assert_eq!(flash_size, "4MB");
    assert_eq!(cache.flash_size, "4MB");
}

/// Regression test: `build_link_args` always emits `-Wl,-Map=` next to
/// `firmware.elf`. ESP32 was the only platform linker not emitting the
/// map before #491 / #508; without it `fbuild bloat` cannot attribute
/// symbols to their source archives.
#[test]
fn test_esp32_link_command_emits_linker_map_next_to_elf() {
    let linker = test_linker("esp32c6");
    let args = linker.build_link_args(
        &[],
        &[],
        &PathBuf::from("/build/firmware.elf"),
        &LinkExtraArgs::default(),
    );
    assert!(
        args.iter().any(|a| a == "-Wl,-Map=/build/firmware.map"),
        "expected -Wl,-Map=/build/firmware.map next to firmware.elf. Args: {:?}",
        args,
    );
}

#[test]
fn legacy_esp32s3_link_includes_cpp_runtime_once() {
    let mut linker = test_linker("esp32s3");
    linker.sdk_lib_flags.clear();
    let args = linker.build_link_args(
        &[],
        &[],
        Path::new("/build/firmware.elf"),
        &LinkExtraArgs::default(),
    );
    assert_eq!(args.iter().filter(|arg| *arg == "-lstdc++").count(), 1);
    let runtime = args.iter().position(|arg| arg == "-lstdc++").unwrap();
    let end_group = args
        .iter()
        .position(|arg| arg == "-Wl,--end-group")
        .unwrap();
    assert!(runtime < end_group);

    linker.sdk_lib_flags.push("-lstdc++".to_string());
    let args = linker.build_link_args(
        &[],
        &[],
        Path::new("/build/firmware.elf"),
        &LinkExtraArgs::default(),
    );
    assert_eq!(args.iter().filter(|arg| *arg == "-lstdc++").count(), 1);
}

#[test]
fn test_linker_flags_use_sdk_ld_flags() {
    let linker = test_linker("esp32c6");
    let flags = linker.linker_flags();
    // SDK ld_flags take priority — profile link flags are skipped
    assert!(flags.contains(&"-nostartfiles".to_string()));
    assert!(flags.contains(&"-u".to_string()));
    assert!(flags.contains(&"app_main".to_string()));
    assert!(flags.contains(&"-Wl,--gc-sections".to_string()));
    // Profile link flags should NOT be present when SDK flags are used
    assert!(!flags.contains(&"-flto=auto".to_string()));
}

#[test]
fn test_linker_flags_fallback_to_config() {
    let config = get_mcu_config("esp32c6").unwrap();
    let prefix = config.toolchain_prefix();
    // Empty sdk_ld_flags → falls back to MCU config
    let linker = Esp32Linker::new(
        PathBuf::from(format!("/usr/bin/{}gcc", prefix)),
        PathBuf::from(format!("/usr/bin/{}ar", prefix)),
        PathBuf::from(format!("/usr/bin/{}objcopy", prefix)),
        PathBuf::from(format!("/usr/bin/{}size", prefix)),
        config,
        vec![],
        vec!["-lfreertos".to_string()],
        LinkerScripts::from_raw_flags(&["-Tmemory.ld".to_string()]),
        BuildProfile::Release,
        None,
        "80m",
        Some(3145728),
        Some(327680),
        None,
        false,
    );
    let flags = linker.linker_flags();
    assert!(flags.iter().any(|f| f.contains("IDF_TARGET_ESP32C6")));
    assert!(flags.contains(&"-fno-rtti".to_string()));
}

#[test]
fn test_sdk_script_flags() {
    let linker = test_linker("esp32c6");
    let args = linker.linker_scripts.to_args();
    assert!(args.iter().any(|f| f.starts_with("-L")));
    assert!(args.iter().any(|f| f == "-Tmemory.ld"));
    assert!(args.iter().any(|f| f == "-Tsections.ld"));
}

#[test]
fn test_sdk_lib_flags_stored() {
    let linker = test_linker("esp32c6");
    assert!(linker.sdk_lib_flags.iter().any(|f| f == "-lfreertos"));
    assert!(linker.sdk_lib_flags.iter().any(|f| f == "-lesp_system"));
    assert!(linker.sdk_lib_flags.iter().any(|f| f.starts_with("-L")));
}

#[test]
fn test_xtensa_linker_flags() {
    // Xtensa with SDK flags that include -mlongcalls
    let config = get_mcu_config("esp32").unwrap();
    let prefix = config.toolchain_prefix();
    let linker = Esp32Linker::new(
        PathBuf::from(format!("/usr/bin/{}gcc", prefix)),
        PathBuf::from(format!("/usr/bin/{}ar", prefix)),
        PathBuf::from(format!("/usr/bin/{}objcopy", prefix)),
        PathBuf::from(format!("/usr/bin/{}size", prefix)),
        config,
        vec!["-mlongcalls".to_string()],
        vec![],
        LinkerScripts::new(),
        BuildProfile::Release,
        None,
        "80m",
        Some(3145728),
        Some(327680),
        None,
        false,
    );
    let flags = linker.linker_flags();
    assert!(flags.contains(&"-mlongcalls".to_string()));
}

#[test]
fn test_bin_output_format() {
    // Verify convert_firmware produces .bin, not .hex
    let linker = test_linker("esp32c6");
    // We can't actually run objcopy, but we can verify the method exists
    // and the linker is properly configured
    assert!(
        linker
            .mcu_config
            .esptool
            .flash_offsets
            .firmware
            .starts_with("0x")
    );
}

/// FastLED/fbuild#1220: during the #1217 outage esptool 5.1.0 WAS
/// installed — it just wasn't on the daemon's PATH — and the build told
/// the user to `pip install esptool`. The message must never say that
/// again, in either branch.
#[test]
fn esptool_spawn_failure_never_recommends_pip_install() {
    let provisioned = esptool_spawn_failure_message(Some(Path::new("/cache/esptool")), "ENOENT");
    let fallback = esptool_spawn_failure_message(None, "ENOENT");

    for msg in [&provisioned, &fallback] {
        assert!(!msg.contains("pip install"), "{msg}");
        assert!(msg.contains("FBUILD_ESPTOOL_PATH"), "{msg}");
        assert!(msg.contains("ENOENT"), "{msg}");
    }
}

/// The provisioned branch is a *different fault* from the PATH-fallback
/// branch and must not claim provisioning failed.
#[test]
fn esptool_spawn_failure_distinguishes_provisioned_from_fallback() {
    let provisioned = esptool_spawn_failure_message(Some(Path::new("/cache/esptool")), "boom");
    assert!(provisioned.contains("/cache/esptool"), "{provisioned}");
    assert!(
        !provisioned.contains("provisioning failed"),
        "{provisioned}"
    );

    let fallback = esptool_spawn_failure_message(None, "boom");
    assert!(fallback.contains("provisioning failed"), "{fallback}");
    assert!(fallback.contains("on PATH"), "{fallback}");
}

#[test]
fn test_f_flash_to_esptool_freq_all_mappings() {
    assert_eq!(f_flash_to_esptool_freq(Some("80000000L"), "40m"), "80m");
    assert_eq!(f_flash_to_esptool_freq(Some("60000000L"), "40m"), "60m");
    assert_eq!(f_flash_to_esptool_freq(Some("40000000L"), "80m"), "40m");
    assert_eq!(f_flash_to_esptool_freq(Some("30000000L"), "80m"), "30m");
    assert_eq!(f_flash_to_esptool_freq(Some("26000000L"), "80m"), "26m");
    assert_eq!(f_flash_to_esptool_freq(Some("20000000L"), "80m"), "20m");
    assert_eq!(f_flash_to_esptool_freq(Some("15000000L"), "80m"), "15m");
    // Invalid esptool frequency falls back to default
    assert_eq!(f_flash_to_esptool_freq(Some("99000000L"), "40m"), "40m");
    assert_eq!(f_flash_to_esptool_freq(Some("64000000L"), "48m"), "48m");
    // Non-numeric falls back to default
    assert_eq!(f_flash_to_esptool_freq(Some("unknown"), "40m"), "40m");
    // None falls back to default
    assert_eq!(f_flash_to_esptool_freq(None, "60m"), "60m");
}

/// ESP32-C2 only supports 60m, 30m, 20m, 15m flash frequencies (not 80m).
/// The board config specifies f_flash=60000000L, so the resolved frequency
/// must be "60m", not "80m".
#[test]
fn test_esp32c2_flash_freq_not_80m() {
    let config = get_mcu_config("esp32c2").unwrap();
    // Default must not be 80m — ESP32-C2 doesn't support it
    assert_ne!(
        config.default_flash_freq(),
        "80m",
        "ESP32-C2 does not support 80m flash frequency"
    );
    assert_eq!(config.default_flash_freq(), "60m");

    // Simulate what the orchestrator does: board has f_flash=60000000L
    let freq = f_flash_to_esptool_freq(Some("60000000L"), config.default_flash_freq());
    assert_eq!(freq, "60m");
}

/// ESP32-H2 board has f_flash=64000000L, but 64m is not a valid esptool frequency.
/// Must fall back to the MCU default of 48m.
#[test]
fn test_esp32h2_flash_freq_not_64m() {
    let config = get_mcu_config("esp32h2").unwrap();
    assert_eq!(config.default_flash_freq(), "48m");

    // Board has f_flash=64000000L → 64m is invalid → falls back to 48m
    let freq = f_flash_to_esptool_freq(Some("64000000L"), config.default_flash_freq());
    assert_eq!(freq, "48m");
}

/// The link runs under the C locale; see [`super::LINK_ENV`].
#[tokio::test]
async fn link_runs_the_linker_under_the_c_locale() {
    if fbuild_core::platform::host::is_windows() {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let recorded = tmp.path().join("lc_all");
    let fake_gcc = tmp.path().join("fake-gcc");
    // Staged and renamed into place: exec'ing a file another thread's fork
    // may still hold open for writing fails with ETXTBSY.
    let staging = tmp.path().join("fake-gcc.staging");
    std::fs::write(
        &staging,
        format!(
            "#!/bin/sh\nprintf '%s' \"$LC_ALL\" > '{}'\n",
            recorded.display()
        ),
    )
    .unwrap();
    fbuild_core::platform::fs::set_executable(&staging).unwrap();
    std::fs::rename(&staging, &fake_gcc).unwrap();
    let mut linker = test_linker("esp32s3");
    linker.gcc_path = fake_gcc;

    linker
        .link(&[], &[], &tmp.path().join("out"), &LinkExtraArgs::default())
        .await
        .unwrap();

    assert_eq!(std::fs::read_to_string(&recorded).unwrap(), "C");
}
