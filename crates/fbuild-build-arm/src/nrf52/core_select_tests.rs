//! Tests for nRF52 Arduino core selection (FastLED/fbuild#2017).

use std::collections::HashMap;
use std::path::Path;

use fbuild_packages::library::Nrf52Framework;

use super::*;

fn board(board_id: &str) -> fbuild_config::BoardConfig {
    fbuild_config::BoardConfig::from_board_id(board_id, &HashMap::new()).unwrap()
}

/// The core's PlatformIO package name and its pinned download.
fn framework_package(board_id: &str) -> (&'static str, fbuild_packages::PackageInfo) {
    let temp = tempfile::TempDir::new().unwrap();
    let framework = selected_framework(&board(board_id)).unwrap();
    let cores = fbuild_packages::library::Nrf52Cores::for_framework(temp.path(), framework);
    (
        framework.package_name(),
        fbuild_packages::Package::get_info(&cores),
    )
}

#[test]
fn xiao_boards_resolve_to_seeed_core() {
    for board_id in ["xiaoble_adafruit", "xiaoblesense_adafruit"] {
        let (package, info) = framework_package(board_id);
        assert_eq!(
            package, "framework-arduinoadafruitnrf52-seeed",
            "{board_id}"
        );
        assert_eq!(info.version, "1.1.13", "{board_id}");
        assert_eq!(
            info.url,
            "https://files.seeedstudio.com/arduino/core/nRF52/Seeed_nRF52_Boards-1.1.13.tar.bz2",
            "{board_id}"
        );
        assert_eq!(
            info.checksum.as_deref(),
            Some("5243bd456e8b7a3328725e17e6c6efc9712662f5aa27b7dc807911aeb0e93071"),
            "{board_id}"
        );
    }
}

#[test]
fn adafruit_boards_still_resolve_to_adafruit_core() {
    for board_id in [
        "adafruit_feather_nrf52840",
        "nrf52840_dk",
        "nrf52840_dk_adafruit",
    ] {
        let (package, info) = framework_package(board_id);
        assert_eq!(package, "framework-arduinoadafruitnrf52", "{board_id}");
        assert_eq!(info.version, "1.10601.0", "{board_id}");
    }
}

#[test]
fn seeed_and_adafruit_cores_cache_separately() {
    let (_, seeed) = framework_package("xiaoble_adafruit");
    let (_, adafruit) = framework_package("adafruit_feather_nrf52840");
    assert_ne!(seeed.name, adafruit.name);
}

#[test]
fn unknown_framework_package_is_a_config_error() {
    let mut board = board("adafruit_feather_nrf52840");
    board.framework_package = Some("framework-arduinonordicnrf5".into());
    let err = selected_framework(&board).unwrap_err().to_string();
    assert!(err.contains("framework-arduinonordicnrf5"), "{err}");
    assert!(
        err.contains("framework-arduinoadafruitnrf52-seeed"),
        "{err}"
    );
}

#[test]
fn board_build_override_selects_the_core() {
    let overrides = HashMap::from([(
        "framework_package".to_string(),
        "framework-arduinoadafruitnrf52-seeed".to_string(),
    )]);
    let board =
        fbuild_config::BoardConfig::from_board_id("adafruit_feather_nrf52840", &overrides).unwrap();
    assert_eq!(selected_framework(&board).unwrap(), Nrf52Framework::Seeed);
}

fn softdevice_tree(versions: &[&str]) -> tempfile::TempDir {
    let temp = tempfile::TempDir::new().unwrap();
    for version in versions {
        std::fs::create_dir_all(temp.path().join("softdevice").join(version)).unwrap();
    }
    temp
}

#[test]
fn v7_linker_script_selects_s140_7_api_headers() {
    let nordic = softdevice_tree(&[
        "s132_nrf52_6.1.1_API",
        "s140_nrf52_6.1.1_API",
        "s140_nrf52_7.3.0_API",
    ]);
    let dir = softdevice_api_dir(nordic.path(), Path::new("/x/nrf52840_s140_v7.ld"));
    assert_eq!(dir, nordic.path().join("softdevice/s140_nrf52_7.3.0_API"));
}

#[test]
fn v6_linker_script_keeps_s140_6_1_1_api_headers() {
    let nordic = softdevice_tree(&["s140_nrf52_6.1.1_API", "s140_nrf52_7.3.0_API"]);
    let dir = softdevice_api_dir(nordic.path(), Path::new("/x/nrf52840_s140_v6.ld"));
    assert_eq!(dir, nordic.path().join("softdevice/s140_nrf52_6.1.1_API"));
}

#[test]
fn newest_matching_softdevice_release_wins() {
    let nordic = softdevice_tree(&["s140_nrf52_7.2.0_API", "s140_nrf52_7.10.0_API"]);
    let dir = softdevice_api_dir(nordic.path(), Path::new("nrf52840_s140_v7.ld"));
    assert_eq!(dir, nordic.path().join("softdevice/s140_nrf52_7.10.0_API"));
}

#[test]
fn unversioned_or_missing_softdevice_falls_back_to_default() {
    let nordic = softdevice_tree(&["s140_nrf52_6.1.1_API"]);
    let default = nordic.path().join("softdevice/s140_nrf52_6.1.1_API");
    assert_eq!(
        softdevice_api_dir(nordic.path(), Path::new("nrf52_xxaa.ld")),
        default
    );
    // Adafruit 1.10601.0 ships no 7.x headers.
    assert_eq!(
        softdevice_api_dir(nordic.path(), Path::new("nrf52840_s140_v7.ld")),
        default
    );
}

fn mdk(name: &str) -> std::path::PathBuf {
    Path::new("/core/nRF5/nordic/nrfx/mdk").join(name)
}

#[test]
fn only_the_mcus_mdk_system_file_is_compiled() {
    let other = Path::new("/core/nRF5/wiring.c").to_path_buf();
    let mut sources = vec![
        mdk("system_nrf52.c"),
        mdk("system_nrf52840.c"),
        other.clone(),
    ];
    retain_mcu_system_source(&mut sources, "NRF52840");
    assert_eq!(sources, vec![mdk("system_nrf52840.c"), other]);
}

#[test]
fn system_files_are_kept_when_none_matches_the_mcu() {
    let mut sources = vec![mdk("system_nrf52.c"), mdk("system_nrf52840.c")];
    retain_mcu_system_source(&mut sources, "nrf52833");
    assert_eq!(sources.len(), 2);
}

#[test]
fn system_named_files_outside_the_mdk_are_untouched() {
    let elsewhere = Path::new("/core/nRF5/system_nrf52.c").to_path_buf();
    let mut sources = vec![mdk("system_nrf52840.c"), elsewhere.clone()];
    retain_mcu_system_source(&mut sources, "nrf52840");
    assert_eq!(sources, vec![mdk("system_nrf52840.c"), elsewhere]);
}
