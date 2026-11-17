//! Bundle-wide board-data guard (FastLED/fbuild#2039, extends the Renesas
//! guard from #2015).
//!
//! Every bundled board ends in exactly one state:
//!
//! - **usable** — an Arduino board on a platform fbuild builds, with a
//!   `build` block, a `build.core` and a variant (`build.variant`, or the
//!   core-specific `build.arduino.<core>.variant` the CH32V boards use);
//! - **expected** — on [`NO_CORE_EXPECTED`] / [`NO_VARIANT_EXPECTED`], with
//!   the reason the missing field does not matter to the orchestrator;
//! - **rejected** — on [`REJECTED_PLATFORMS`] or [`REJECTED_BOARDS`], and
//!   [`ensure_board_buildable`] fails early with a typed error naming it.
//!
//! Boards that do not list the `arduino` framework (Zephyr-, mbed- or
//! IDF-only entries on a supported platform) carry no Arduino core or
//! variant upstream either; they are outside this guard's core/variant check
//! but still covered by the reject check.

use std::collections::BTreeSet;

use serde_json::Value;

use super::db::get_board_db;
use super::ensure_board_buildable;
use super::support::unsupported_reason;

/// PlatformIO platforms with no fbuild orchestrator. Every bundled board on
/// one of these is rejected at board resolution.
const REJECTED_PLATFORMS: &[&str] = &[
    "Bonfire",
    "aceinna_imu",
    "azadi",
    "chipsalliance",
    "freescalekinetis",
    "heltec-cubecell",
    "infineonxmc",
    "intel_arc32",
    "intel_mcs51",
    "k1921vk",
    "kendryte210",
    "lattice_ice40",
    "lgt8f",
    "libretiny",
    "linux_arm",
    "logicrom",
    "maxim32",
    "microchippic32",
    "nuclei",
    "nxpimxrt",
    "openhw",
    "riscv_gap",
    "shakti",
    "sifive",
    "ststm8",
    "timsp430",
    "timsp432",
    "titiva",
];

/// Boards on a supported platform that fbuild still cannot build. Each one's
/// JSON carries an `unsupported_reason`.
const REJECTED_BOARDS: &[(&str, &str)] = &[
    (
        "portenta_c33",
        "needs the renesas_portenta core and an RA6M5 MCU config (FastLED/fbuild#2054)",
    ),
    ("wasm", "Platform::Wasm has no native orchestrator"),
];

/// `(platform, reason)`: Arduino boards that may omit `build.core`.
const NO_CORE_EXPECTED: &[(&str, &str)] = &[(
    "ststm32",
    "the STM32 orchestrator always compiles cores/arduino of the framework it picks from the \
     variant; upstream PlatformIO's boards carry no build.core either",
)];

/// `(platform, core, reason)`: Arduino boards that may omit `build.variant`.
const NO_VARIANT_EXPECTED: &[(&str, &str, &str)] = &[
    (
        "teensy",
        "teensy",
        "Teensyduino cores have no per-board variants",
    ),
    (
        "teensy",
        "teensy3",
        "Teensyduino cores have no per-board variants",
    ),
    (
        "teensy",
        "teensy4",
        "Teensyduino cores have no per-board variants",
    ),
    (
        "atmelavr",
        "tinymodern",
        "ATTinyCore 1.5.2 boards.txt gives tinymodern boards no build.variant; \
         pins_arduino.h lives in cores/tinymodern",
    ),
];

fn str_field<'a>(value: Option<&'a Value>, key: &str) -> Option<&'a str> {
    value
        .and_then(|v| v.get(key))
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

fn lists_arduino(board: &Value) -> bool {
    board
        .get("frameworks")
        .and_then(Value::as_array)
        .is_some_and(|fws| fws.iter().any(|fw| fw.as_str() == Some("arduino")))
}

fn has_variant(build: &Value, core: Option<&str>) -> bool {
    str_field(Some(build), "variant").is_some()
        || core.is_some_and(|core| {
            str_field(build.get("arduino").and_then(|a| a.get(core)), "variant").is_some()
        })
}

/// Check one non-rejected Arduino board; record any allow-list entry used.
fn check_usable(
    board_id: &str,
    platform: &str,
    board: &Value,
    used: &mut BTreeSet<String>,
) -> Vec<String> {
    let Some(build) = board.get("build").filter(|b| b.is_object()) else {
        return vec![format!("{board_id} ({platform}): no build block")];
    };
    let mut violations = Vec::new();
    let core = str_field(Some(build), "core");
    if core.is_none() {
        match NO_CORE_EXPECTED.iter().find(|(p, _)| *p == platform) {
            Some((p, _)) => {
                used.insert(format!("core:{p}"));
            }
            None => violations.push(format!("{board_id} ({platform}): no build.core")),
        }
    }
    if !has_variant(build, core) {
        let core = core.unwrap_or_default();
        match NO_VARIANT_EXPECTED
            .iter()
            .find(|(p, c, _)| *p == platform && *c == core)
        {
            Some((p, c, _)) => {
                used.insert(format!("variant:{p}/{c}"));
            }
            None => violations.push(format!(
                "{board_id} ({platform}, core {core}): no build.variant"
            )),
        }
    }
    violations
}

#[test]
fn every_bundled_board_is_usable_expected_or_rejected() {
    let mut violations = Vec::new();
    let mut used = BTreeSet::new();
    for (board_id, board) in get_board_db() {
        let platform = board
            .get("platform")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let listed_rejected = REJECTED_PLATFORMS.contains(&platform)
            || REJECTED_BOARDS.iter().any(|(id, _)| id == board_id);
        let rejected = unsupported_reason(board).is_some();
        if rejected != listed_rejected {
            violations.push(format!(
                "{board_id} ({platform}): rejected={rejected} but reject lists say {listed_rejected}"
            ));
        }
        if rejected {
            used.insert(format!("reject:{platform}"));
            used.insert(format!("reject:{board_id}"));
            continue;
        }
        if lists_arduino(board) {
            violations.extend(check_usable(board_id, platform, board, &mut used));
        }
    }
    violations.sort();
    assert!(
        violations.is_empty(),
        "bundled board data incomplete (FastLED/fbuild#2039). Fill the field from upstream, \
         or add the board to an allow-list or reject list with a reason:\n{}",
        violations.join("\n")
    );

    let declared = REJECTED_PLATFORMS
        .iter()
        .map(|p| format!("reject:{p}"))
        .chain(REJECTED_BOARDS.iter().map(|(id, _)| format!("reject:{id}")))
        .chain(NO_CORE_EXPECTED.iter().map(|(p, _)| format!("core:{p}")))
        .chain(
            NO_VARIANT_EXPECTED
                .iter()
                .map(|(p, c, _)| format!("variant:{p}/{c}")),
        );
    let stale: Vec<String> = declared.filter(|key| !used.contains(key)).collect();
    assert!(
        stale.is_empty(),
        "allow-list / reject-list entries no bundled board needs any more: {stale:?}"
    );
}

#[test]
fn portenta_c33_is_rejected_with_the_missing_core() {
    let err = ensure_board_buildable("portenta_c33").unwrap_err();
    assert!(matches!(
        &err,
        fbuild_core::FbuildError::UnsupportedBoard { board, .. } if board == "portenta_c33"
    ));
    assert_eq!(
        err.to_string(),
        "board 'portenta_c33' cannot be built by fbuild: it needs the separate Arduino \
         renesas_portenta core and an RA6M5 MCU config, which fbuild does not support yet \
         (FastLED/fbuild#2054)"
    );
}

#[test]
fn board_on_a_platform_without_orchestrator_is_rejected() {
    let err = ensure_board_buildable("cubecell_board").unwrap_err();
    assert_eq!(
        err.to_string(),
        "board 'cubecell_board' cannot be built by fbuild: its PlatformIO platform \
         'heltec-cubecell' has no fbuild build orchestrator"
    );
    let err = ensure_board_buildable("bw12").unwrap_err();
    assert!(err.to_string().contains("platform 'libretiny'"), "{err}");
}

#[test]
fn wasm_board_is_rejected() {
    let err = ensure_board_buildable("wasm").unwrap_err();
    assert_eq!(
        err.to_string(),
        "board 'wasm' cannot be built by fbuild: fbuild has no WebAssembly (Emscripten) \
         build orchestrator"
    );
}

#[test]
fn buildable_unknown_and_aliased_boards_pass() {
    for board in [
        "uno",
        "teensy41",
        "attiny841",
        "nucleo_l432kc",
        "esp32c6",
        "rpipico",
    ] {
        ensure_board_buildable(board).unwrap_or_else(|e| panic!("{board}: {e}"));
    }
    // Unknown ids are left to board resolution's own error.
    ensure_board_buildable("definitely-not-a-board").unwrap();
}

/// FastLED/fbuild#2039: both C6 boards name their arduino-esp32 variant
/// (platform-espressif32 b306cf3c, arduino-esp32 boards.txt) instead of
/// falling back to the MCU name; the XIAO's pins differ from `esp32c6`.
#[test]
fn esp32c6_boards_name_their_arduino_esp32_variant() {
    use std::collections::HashMap;
    let devkit = super::BoardConfig::from_board_id("esp32-c6-devkitc-1", &HashMap::new()).unwrap();
    assert_eq!(devkit.variant, "esp32c6");
    let xiao = super::BoardConfig::from_board_id("seeed_xiao_esp32c6", &HashMap::new()).unwrap();
    assert_eq!(xiao.variant, "XIAO_ESP32C6");
    let defines = xiao.get_defines();
    for name in [
        "ARDUINO_XIAO_ESP32C6",
        "ARDUINO_USB_MODE",
        "ARDUINO_USB_CDC_ON_BOOT",
    ] {
        assert_eq!(defines.get(name).map(String::as_str), Some("1"), "{name}");
    }
}
