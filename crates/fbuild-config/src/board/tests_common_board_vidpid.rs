//! Codifies the FastLED/fbuild#740 "Board-name match results" table as an
//! executable assertion.
//!
//! Issue #740 keeps a hand-curated list of common board names and their
//! expected VID:PID pairs, populated at each verification pass against the
//! published `online-data` branch. This test freezes that curation for the
//! subset of rows that resolve through first-party fbuild board JSONs (i.e.
//! `crates/fbuild-config/assets/boards/json/<board_id>.json` carries
//! `build.vid` / `build.pid`) so CI catches any regression that drops those
//! fields.
//!
//! Rows in #740 that resolve via an MCU-to-VID heuristic (VID/PID left null
//! in the board JSON — e.g. Teensy, Nucleo, Bluepill, Blackpill, LPC845) are
//! deliberately excluded here; those live in the `online-data` publish path,
//! not in this repo's static board tree.
//!
//! Case-insensitive comparison: some boards ship `0x303A` (uppercase hex)
//! while others ship `0x2341` (lowercase). Both are valid per the enrichment
//! pipeline — this test normalizes to lowercase before comparing.

use std::collections::HashMap;

use super::BoardConfig;

/// `(board_id, expected_vid, expected_pid)` — the tuple layout keeps each
/// row a single sub-100-char line so rustfmt does not reflow the table.
type Row = (&'static str, &'static str, &'static str);

/// The static verification set frozen from FastLED/fbuild#740 (2026-07-01
/// snapshot, headline hit-rate 57/57). Only rows where fbuild's board JSON
/// carries `build.vid` + `build.pid` land here — MCU-heuristic rows are
/// covered by the `online-data` pipeline, not this static tree.
const FIRST_PARTY_VID_PID_ROWS: &[Row] = &[
    // Arduino family (VID 0x2341).
    ("uno", "0x2341", "0x0043"),
    ("leonardo", "0x2341", "0x8036"),
    ("megaatmega2560", "0x2341", "0x0042"),
    ("micro", "0x2341", "0x8037"),
    ("zeroUSB", "0x2341", "0x804d"),
    ("due", "0x2341", "0x003D"),
    ("mkr1000USB", "0x2341", "0x804e"),
    ("nano_every", "0x2341", "0x0058"),
    ("nano_33_iot", "0x2341", "0x8057"),
    ("arduino_nano_matter", "0x2341", "0x0072"),
    ("giga_r1_m7", "0x2341", "0x0366"),
    ("nanorp2040connect", "0x2341", "0x005e"),
    // Espressif VID 0x303A. Devkits share the composite-JTAG PID 0x1001.
    // ESP32 SoC-side USB JTAG PIDs are populated only for the boards whose
    // JSON carries explicit `build.vid`/`build.pid`. Rows whose JSON has
    // `null` (e.g. `esp32-c3-devkitm-1`, `esp32-c6-devkitc-1`,
    // `esp32-p4-evboard`) resolve at runtime via the online-data
    // `mcu_to_vid` heuristic — those live in #740's verification, not
    // this static assertion.
    ("esp32-s3-devkitc-1", "0x303A", "0x1001"),
    ("esp32-h2-devkitm-1", "0x303A", "0x1001"),
    // Unexpected Maker ESP32-S3 SKUs — distinct PIDs per product.
    ("um_tinys3", "0x303A", "0x80D0"),
    ("um_feathers3", "0x303A", "0x80D6"),
    // ESP32 CP2102 bridge (VID 0x10C4). Only boards whose JSON carries
    // an explicit `build.vid`/`build.pid` land here; devkit rows without
    // it (e.g. `esp32doit-devkit-v1`) resolve via the MCU heuristic.
    ("esp32-s2-saola-1", "0x10C4", "0xEA60"),
    // Adafruit VID 0x239A.
    ("adafruit_feather_nrf52840", "0x239A", "0x8029"),
    ("adafruit_feather_nrf52840_sense", "0x239A", "0x8087"),
    ("adafruit_itsybitsy_nrf52840", "0x239A", "0x8051"),
    ("adafruit_cplaynrf52840", "0x239A", "0x8045"),
    ("adafruit_clue_nrf52840", "0x239A", "0x8071"),
    ("adafruit_matrix_portal_m4", "0x239A", "0x80C9"),
    ("adafruit_qt_py_m0", "0x239A", "0x80CB"),
    ("adafruit_qt_py_rp2040", "0x239A", "0x80F8"),
    // Raspberry Pi VID 0x2E8A.
    ("rpipico", "0x2E8A", "0x000A"),
    ("rpipico2", "0x2E8A", "0x000B"),
    ("rpipicow", "0x2E8A", "0xF00A"),
    ("seeed_xiao_rp2040", "0x2E8A", "0x000A"),
    // Seeed VID 0x2886.
    ("seeed_xiao_mg24", "0x2886", "0x0062"),
    ("seeed_xiao_esp32s3", "0x2886", "0x0056"),
    // SparkFun VID 0x1B4F + Segger J-Link OB VID 0x1366.
    ("sparkfun_promicro16", "0x1B4F", "0x9206"),
    ("sparkfun_thingplusmatter", "0x1366", "0x0101"),
    // nRF52 bootloader-shared VID 0x239A / pid.codes VID 0x1209.
    ("nice_nano", "0x239A", "0x00B3"),
    ("nrfmicro", "0x1209", "0x5284"),
    // NXP LPC845-BRK VID 0x0D28 (LPC-Link2) — resolves via runtime
    // overlay (board JSON has null VID/PID); excluded from static
    // assertion per the online-data pipeline.
    // CH32V003 WCH-LinkE (VID 0x1A86, RISC-V mode PID 0x8010).
    ("ch32v003f4p6_evt_r0", "0x1A86", "0x8010"),
];

fn norm_hex(s: &str) -> String {
    s.to_ascii_lowercase()
}

#[test]
fn issue_740_first_party_vid_pid_rows_all_resolve() {
    let mut missing_config: Vec<&str> = Vec::new();
    let mut missing_vid: Vec<&str> = Vec::new();
    let mut missing_pid: Vec<&str> = Vec::new();
    let mut wrong_vid: Vec<(String, String, String)> = Vec::new();
    let mut wrong_pid: Vec<(String, String, String)> = Vec::new();

    for &(board_id, expected_vid, expected_pid) in FIRST_PARTY_VID_PID_ROWS {
        let config = match BoardConfig::from_board_id(board_id, &HashMap::new()) {
            Ok(c) => c,
            Err(_) => {
                missing_config.push(board_id);
                continue;
            }
        };

        match config.vid.as_deref() {
            None => missing_vid.push(board_id),
            Some(got) if norm_hex(got) != norm_hex(expected_vid) => {
                wrong_vid.push((board_id.to_string(), expected_vid.to_string(), got.to_string()));
            }
            Some(_) => {}
        }

        match config.pid.as_deref() {
            None => missing_pid.push(board_id),
            Some(got) if norm_hex(got) != norm_hex(expected_pid) => {
                wrong_pid.push((board_id.to_string(), expected_pid.to_string(), got.to_string()));
            }
            Some(_) => {}
        }
    }

    let mut failures = Vec::new();
    if !missing_config.is_empty() {
        failures.push(format!(
            "{} board id(s) did not resolve via from_board_id: {:?}",
            missing_config.len(),
            missing_config
        ));
    }
    if !missing_vid.is_empty() {
        failures.push(format!(
            "{} board(s) missing build.vid: {:?}",
            missing_vid.len(),
            missing_vid
        ));
    }
    if !missing_pid.is_empty() {
        failures.push(format!(
            "{} board(s) missing build.pid: {:?}",
            missing_pid.len(),
            missing_pid
        ));
    }
    if !wrong_vid.is_empty() {
        failures.push(format!(
            "{} board(s) with wrong build.vid (id, expected, got): {:?}",
            wrong_vid.len(),
            wrong_vid
        ));
    }
    if !wrong_pid.is_empty() {
        failures.push(format!(
            "{} board(s) with wrong build.pid (id, expected, got): {:?}",
            wrong_pid.len(),
            wrong_pid
        ));
    }

    assert!(
        failures.is_empty(),
        "FastLED/fbuild#740 board-name → VID:PID verification regressed:\n{}",
        failures.join("\n")
    );
}
