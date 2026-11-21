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

/// `portenta_c33.build.variant=PORTENTA_C33` in the separate
/// `arduino:renesas_portenta` 1.6.0 package (the board is an RA6M5, not the
/// RA4M1 the UNO R4 boards use). FastLED/fbuild#2054 moved this board off the
/// reject list, so its variant/core/defines must match that package's
/// `boards.txt` exactly.
#[test]
fn portenta_c33_resolves_the_portenta_variant_and_defines() {
    let config = BoardConfig::from_board_id("portenta_c33", &HashMap::new()).unwrap();
    assert_eq!(config.variant, "PORTENTA_C33");
    assert_eq!(config.core, "arduino");
    // BoardConfig normalizes `mcu` to lower case; the builder maps it back to
    // the RA6M5 family (see `renesas_mcu_family`).
    assert_eq!(config.mcu, "r7fa6m5bh2cbg");
    assert_eq!(config.platform(), Some(fbuild_core::Platform::RenesasRa));
    assert_defines(
        "portenta_c33",
        &config,
        &[
            "F_CPU",
            "ARDUINO_PORTENTA_C33",
            "ARDUINO_ARCH_RENESAS",
            "ARDUINO_ARCH_RENESAS_PORTENTA",
            "ARDUINO_FSP",
            "CFG_TUSB_MCU",
            "_RA_CORE",
            "_RENESAS_RA_",
        ],
        &["NO_USB", "ARDUINO_UNOWIFIR4", "ARDUINO_ARCH_RENESAS_UNO"],
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

/// Pinned upstream `upload` block for the RA boards that flash through the
/// DFU bootloader (`(board_id, maximum_ram_size, maximum_size)`).
///
/// Values are PlatformIO `platform-renesas-ra` `develop` board JSON fields
/// (`platformio/platform-renesas-ra`, `boards/{portenta_c33,uno_r4_minima,nano_r4}.json`),
/// which is the same source `enrich_boards` and `ci/validate_boards.py`
/// compare against. Only fields `enrich_boards` emits are pinned here:
/// `native_usb`, `protocols` and `offset_address` are not in its
/// `UPLOAD_FIELDS`, so adding them would read as drift.
const RA_DFU_UPLOAD_BLOCK: &[(&str, u64, u64)] = &[
    ("portenta_c33", 523_624, 2_097_152),
    ("uno_r4_minima", 32_768, 262_144),
    ("nano_r4", 32_768, 262_144),
];

/// The Renesas RA platform has no deployer yet (FastLED/fbuild#2131 stage 3),
/// but the board data it will read must be right first: without an `upload`
/// block `upload.protocol` is `None`, so the platform arm has nothing to
/// dispatch on and the size checks cannot tell a DFU image from anything else.
#[test]
fn ra_dfu_boards_declare_the_upstream_upload_block() {
    let db = get_board_db();
    for (board_id, expected_ram, expected_size) in RA_DFU_UPLOAD_BLOCK {
        let board = db
            .get(*board_id)
            .unwrap_or_else(|| panic!("{board_id}: not in the bundled board db"));
        let upload = board.get("upload").unwrap_or_else(|| {
            panic!("{board_id}: missing the upload block (FastLED/fbuild#2131)")
        });
        assert_eq!(
            upload.get("protocol").and_then(Value::as_str),
            Some("dfu"),
            "{board_id}: upload.protocol must match platform-renesas-ra"
        );
        assert_eq!(
            upload.get("maximum_ram_size").and_then(Value::as_u64),
            Some(*expected_ram),
            "{board_id}: upload.maximum_ram_size must match platform-renesas-ra"
        );
        assert_eq!(
            upload.get("maximum_size").and_then(Value::as_u64),
            Some(*expected_size),
            "{board_id}: upload.maximum_size must match platform-renesas-ra"
        );
        // The two spellings of the same number must not drift apart: the
        // bundle's top-level ram/rom are what the size check compares against.
        assert_eq!(
            board.get("ram").and_then(Value::as_u64),
            upload.get("maximum_ram_size").and_then(Value::as_u64),
            "{board_id}: top-level ram and upload.maximum_ram_size disagree"
        );
        assert_eq!(
            board.get("rom").and_then(Value::as_u64),
            upload.get("maximum_size").and_then(Value::as_u64),
            "{board_id}: top-level rom and upload.maximum_size disagree"
        );
        // Bundled snapshots stay hwids-free (ci/check_usb_vidpid_literals.py).
        assert!(
            !upload.to_string().contains("hwids"),
            "{board_id}: the upload block must not carry hwids"
        );
    }
}

/// The production loader must surface the block: `BoardConfig::from_board_id`
/// is the entry point every deploy/build path uses, so asserting on the raw
/// JSON alone would prove the file, not the wiring.
#[test]
fn ra_dfu_boards_surface_upload_protocol_through_the_loader() {
    for (board_id, _, _) in RA_DFU_UPLOAD_BLOCK {
        let config = BoardConfig::from_board_id(board_id, &HashMap::new()).unwrap();
        assert_eq!(
            config.upload_protocol.as_deref(),
            Some("dfu"),
            "{board_id}: BoardConfig must surface upload.protocol"
        );
    }
}
