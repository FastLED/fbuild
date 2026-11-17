//! Renesas RA board-definition guards (FastLED/fbuild#2015).
//!
//! The Renesas builder takes the FSP include directories (`bsp_api.h`,
//! `common_data.h`, ...) from `variants/<variant>/includes.txt` in
//! ArduinoCore-renesas. A bundled board JSON without a `build` block resolves
//! to the generic `standard` variant, which does not exist in that core, so
//! the core fails to compile. These tests pin each Renesas board to the
//! variant and defines the pinned core's `boards.txt` gives it.

use std::collections::HashMap;

use serde_json::Value;

use super::BoardConfig;
use super::db::get_board_db;

/// Split a board's `extra_flags` into the macro names it defines.
fn extra_flag_defines(config: &BoardConfig) -> Vec<String> {
    config
        .extra_flags
        .as_deref()
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|flag| flag.strip_prefix("-D"))
        .map(|raw| {
            raw.split_once('=')
                .map_or(raw, |(name, _)| name)
                .to_string()
        })
        .collect()
}

fn assert_defines(board_id: &str, config: &BoardConfig, required: &[&str], forbidden: &[&str]) {
    let defines = extra_flag_defines(config);
    for name in required {
        assert!(
            defines.iter().any(|d| d == name),
            "{board_id}: extra_flags must define {name}, got {defines:?}"
        );
    }
    for name in forbidden {
        assert!(
            !defines.iter().any(|d| d == name),
            "{board_id}: extra_flags must not define {name}, got {defines:?}"
        );
    }
}

/// `minima.build.variant=MINIMA` in ArduinoCore-renesas 1.6.0. The core's
/// own sources branch on `ARDUINO_MINIMA` (`-DARDUINO_{build.board}`), and the
/// Minima has native USB, so the WiFi board's `NO_USB` must not leak in.
#[test]
fn uno_r4_minima_resolves_the_minima_variant_and_defines() {
    let config = BoardConfig::from_board_id("uno_r4_minima", &HashMap::new()).unwrap();
    assert_eq!(config.variant, "MINIMA");
    assert_eq!(config.core, "arduino");
    assert_eq!(config.f_cpu, "48000000L");
    assert_eq!(config.platform(), Some(fbuild_core::Platform::RenesasRa));
    assert_defines(
        "uno_r4_minima",
        &config,
        &[
            "F_CPU",
            "ARDUINO_UNOR4_MINIMA",
            "ARDUINO_MINIMA",
            "ARDUINO_ARCH_RENESAS",
            "ARDUINO_ARCH_RENESAS_UNO",
            "ARDUINO_FSP",
            "CFG_TUSB_MCU",
            "_RA_CORE",
            "_RENESAS_RA_",
        ],
        &["NO_USB", "BACKTRACE_SUPPORT", "ARDUINO_UNOR4_WIFI"],
    );
}

/// `unor4wifi.build.variant=UNOWIFIR4` in ArduinoCore-renesas 1.6.0. The
/// core's recipes emit `-DARDUINO_ARCH_{build.arch}`, and the package
/// architecture is `renesas_uno`, so Arduino IDE/CLI define
/// `ARDUINO_ARCH_RENESAS_UNO` alongside the literal `ARDUINO_ARCH_RENESAS`.
/// PlatformIO's `uno_r4_wifi.json` carries it too (FastLED/fbuild#2038).
#[test]
fn uno_r4_wifi_resolves_the_unowifir4_variant_and_defines() {
    let config = BoardConfig::from_board_id("uno_r4_wifi", &HashMap::new()).unwrap();
    assert_eq!(config.variant, "UNOWIFIR4");
    assert_eq!(config.core, "arduino");
    assert_eq!(config.platform(), Some(fbuild_core::Platform::RenesasRa));
    assert_defines(
        "uno_r4_wifi",
        &config,
        &[
            "F_CPU",
            "ARDUINO_UNOR4_WIFI",
            "ARDUINO_UNOWIFIR4",
            "NO_USB",
            "BACKTRACE_SUPPORT",
            "ARDUINO_ARCH_RENESAS",
            "ARDUINO_ARCH_RENESAS_UNO",
            "ARDUINO_FSP",
            "CFG_TUSB_MCU",
            "_RA_CORE",
            "_RENESAS_RA_",
        ],
        &["ARDUINO_UNOR4_MINIMA", "ARDUINO_MINIMA"],
    );
    let defines = config.get_defines();
    assert_eq!(
        defines.get("ARDUINO_ARCH_RENESAS_UNO").map(String::as_str),
        Some("1")
    );
}

/// `nanor4.build.variant=NANOR4`; its `build.defines` add `ARDUINO_NANO_R4`
/// and `ARDUINO_UNOR4_MINIMA`.
#[test]
fn nano_r4_resolves_the_nanor4_variant_and_defines() {
    let config = BoardConfig::from_board_id("nano_r4", &HashMap::new()).unwrap();
    assert_eq!(config.variant, "NANOR4");
    assert_eq!(config.core, "arduino");
    assert_defines(
        "nano_r4",
        &config,
        &[
            "ARDUINO_NANO_R4",
            "ARDUINO_UNOR4_MINIMA",
            "ARDUINO_ARCH_RENESAS",
            "ARDUINO_FSP",
            "CFG_TUSB_MCU",
            "_RA_CORE",
        ],
        &["NO_USB"],
    );
}

/// Every bundled Renesas RA board must name its ArduinoCore-renesas variant
/// and core explicitly. Falling back to the generic `standard` variant
/// silently drops the FSP include path (FastLED/fbuild#2015).
#[test]
fn every_renesas_board_declares_build_core_and_variant() {
    let mut violations = Vec::new();
    let mut checked = 0;
    for (board_id, board) in get_board_db() {
        if board.get("platform").and_then(Value::as_str) != Some("renesas-ra") {
            continue;
        }
        checked += 1;
        let build = board.get("build");
        for field in ["core", "variant", "extra_flags"] {
            let present = build
                .and_then(|b| b.get(field))
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty());
            if !present {
                violations.push(format!("{board_id}: missing build.{field}"));
            }
        }
    }
    violations.sort();
    assert!(checked > 0, "no renesas-ra boards found in the bundle");
    assert!(
        violations.is_empty(),
        "Renesas boards need an explicit build block (FastLED/fbuild#2015):\n{}",
        violations.join("\n")
    );
}
