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

/// One row from #740's Board-name match results table that ships a real
/// `build.vid` / `build.pid` in the first-party board JSON.
struct BoardVidPidRow {
    /// The `board_id` column from the table — matches the JSON filename
    /// under `assets/boards/json/<board_id>.json`.
    board_id: &'static str,
    /// Expected `build.vid` as a 4-hex value with `0x` prefix.
    expected_vid: &'static str,
    /// Expected `build.pid` as a 4-hex value with `0x` prefix.
    expected_pid: &'static str,
}

/// The static verification set frozen from FastLED/fbuild#740 (2026-07-01
/// snapshot, headline hit-rate 57/57). Only rows where fbuild's board JSON
/// carries `build.vid` + `build.pid` land here — MCU-heuristic rows are
/// covered by the `online-data` pipeline, not this static tree.
const FIRST_PARTY_VID_PID_ROWS: &[BoardVidPidRow] = &[
    // Arduino family (VID 0x2341 / 0x2A03 for older SKUs).
    BoardVidPidRow {
        board_id: "uno",
        expected_vid: "0x2341",
        expected_pid: "0x0043",
    },
    BoardVidPidRow {
        board_id: "leonardo",
        expected_vid: "0x2341",
        expected_pid: "0x8036",
    },
    BoardVidPidRow {
        board_id: "megaatmega2560",
        expected_vid: "0x2341",
        expected_pid: "0x0042",
    },
    BoardVidPidRow {
        board_id: "micro",
        expected_vid: "0x2341",
        expected_pid: "0x8037",
    },
    BoardVidPidRow {
        board_id: "zeroUSB",
        expected_vid: "0x2341",
        expected_pid: "0x804d",
    },
    BoardVidPidRow {
        board_id: "due",
        expected_vid: "0x2341",
        expected_pid: "0x003D",
    },
    BoardVidPidRow {
        board_id: "mkr1000USB",
        expected_vid: "0x2341",
        expected_pid: "0x804e",
    },
    BoardVidPidRow {
        board_id: "nano_every",
        expected_vid: "0x2341",
        expected_pid: "0x0058",
    },
    BoardVidPidRow {
        board_id: "nano_33_iot",
        expected_vid: "0x2341",
        expected_pid: "0x8057",
    },
    BoardVidPidRow {
        board_id: "arduino_nano_matter",
        expected_vid: "0x2341",
        expected_pid: "0x0072",
    },
    BoardVidPidRow {
        board_id: "giga_r1_m7",
        expected_vid: "0x2341",
        expected_pid: "0x0366",
    },
    BoardVidPidRow {
        board_id: "nanorp2040connect",
        expected_vid: "0x2341",
        expected_pid: "0x005e",
    },
    // uno_r4_wifi resolves via the online-data `mcu_to_vid` heuristic —
    // its board JSON has null vid/pid. Covered by the online-data
    // publish, not this static tree. (FastLED/fbuild#740 follow-up.)
    //
    // Espressif VID 0x303A. Devkits share the composite-JTAG PID 0x1001.
    // Only rows whose JSON ships an explicit `build.vid`/`build.pid`
    // land here; devkits whose JSON has `null` (esp32-c3-devkitm-1,
    // esp32-c6-devkitc-1, esp32-p4-evboard) resolve via the runtime
    // mcu_to_vid heuristic and are covered by the online-data pipeline.
    BoardVidPidRow {
        board_id: "esp32-s3-devkitc-1",
        expected_vid: "0x303A",
        expected_pid: "0x1001",
    },
    BoardVidPidRow {
        board_id: "esp32-h2-devkitm-1",
        expected_vid: "0x303A",
        expected_pid: "0x1001",
    },
    // Unexpected Maker ESP32-S3 SKUs — distinct PIDs per product.
    BoardVidPidRow {
        board_id: "um_tinys3",
        expected_vid: "0x303A",
        expected_pid: "0x80D0",
    },
    BoardVidPidRow {
        board_id: "um_feathers3",
        expected_vid: "0x303A",
        expected_pid: "0x80D6",
    },
    // ESP32 CP2102 bridge (VID 0x10C4). esp32doit-devkit-v1 resolves
    // via the MCU heuristic (board JSON has null vid/pid).
    BoardVidPidRow {
        board_id: "esp32-s2-saola-1",
        expected_vid: "0x10C4",
        expected_pid: "0xEA60",
    },
    // Adafruit VID 0x239A.
    BoardVidPidRow {
        board_id: "adafruit_feather_nrf52840",
        expected_vid: "0x239A",
        expected_pid: "0x8029",
    },
    BoardVidPidRow {
        board_id: "adafruit_feather_nrf52840_sense",
        expected_vid: "0x239A",
        expected_pid: "0x8087",
    },
    BoardVidPidRow {
        board_id: "adafruit_itsybitsy_nrf52840",
        expected_vid: "0x239A",
        expected_pid: "0x8051",
    },
    BoardVidPidRow {
        board_id: "adafruit_cplaynrf52840",
        expected_vid: "0x239A",
        expected_pid: "0x8045",
    },
    BoardVidPidRow {
        board_id: "adafruit_clue_nrf52840",
        expected_vid: "0x239A",
        expected_pid: "0x8071",
    },
    BoardVidPidRow {
        board_id: "adafruit_matrix_portal_m4",
        expected_vid: "0x239A",
        expected_pid: "0x80C9",
    },
    BoardVidPidRow {
        board_id: "adafruit_qt_py_m0",
        expected_vid: "0x239A",
        expected_pid: "0x80CB",
    },
    BoardVidPidRow {
        board_id: "adafruit_qt_py_rp2040",
        expected_vid: "0x239A",
        expected_pid: "0x80F8",
    },
    // Raspberry Pi VID 0x2E8A.
    BoardVidPidRow {
        board_id: "rpipico",
        expected_vid: "0x2E8A",
        expected_pid: "0x000A",
    },
    BoardVidPidRow {
        board_id: "rpipico2",
        expected_vid: "0x2E8A",
        expected_pid: "0x000B",
    },
    BoardVidPidRow {
        board_id: "rpipicow",
        expected_vid: "0x2E8A",
        expected_pid: "0xF00A",
    },
    BoardVidPidRow {
        board_id: "seeed_xiao_rp2040",
        expected_vid: "0x2E8A",
        expected_pid: "0x000A",
    },
    // Seeed VID 0x2886.
    BoardVidPidRow {
        board_id: "seeed_xiao_mg24",
        expected_vid: "0x2886",
        expected_pid: "0x0062",
    },
    BoardVidPidRow {
        board_id: "seeed_xiao_esp32s3",
        expected_vid: "0x2886",
        expected_pid: "0x0056",
    },
    // SparkFun VID 0x1B4F + Segger J-Link OB VID 0x1366.
    BoardVidPidRow {
        board_id: "sparkfun_promicro16",
        expected_vid: "0x1B4F",
        expected_pid: "0x9206",
    },
    BoardVidPidRow {
        board_id: "sparkfun_thingplusmatter",
        expected_vid: "0x1366",
        expected_pid: "0x0101",
    },
    // nRF52 bootloader-shared VID 0x239A / pid.codes VID 0x1209.
    BoardVidPidRow {
        board_id: "nice_nano",
        expected_vid: "0x239A",
        expected_pid: "0x00B3",
    },
    BoardVidPidRow {
        board_id: "nrfmicro",
        expected_vid: "0x1209",
        expected_pid: "0x5284",
    },
    // NXP LPC845-BRK VID 0x0D28 (LPC-Link2) — resolves via the
    // runtime mcu_to_vid overlay; board JSON has null vid/pid. Not a
    // first-party static row.
    // CH32V003 WCH-LinkE (VID 0x1A86, RISC-V mode PID 0x8010).
    BoardVidPidRow {
        board_id: "ch32v003f4p6_evt_r0",
        expected_vid: "0x1A86",
        expected_pid: "0x8010",
    },
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

    for row in FIRST_PARTY_VID_PID_ROWS {
        let config = match BoardConfig::from_board_id(row.board_id, &HashMap::new()) {
            Ok(c) => c,
            Err(_) => {
                missing_config.push(row.board_id);
                continue;
            }
        };

        match config.vid.as_deref() {
            None => missing_vid.push(row.board_id),
            Some(got) if norm_hex(got) != norm_hex(row.expected_vid) => {
                wrong_vid.push((
                    row.board_id.to_string(),
                    row.expected_vid.to_string(),
                    got.to_string(),
                ));
            }
            Some(_) => {}
        }

        match config.pid.as_deref() {
            None => missing_pid.push(row.board_id),
            Some(got) if norm_hex(got) != norm_hex(row.expected_pid) => {
                wrong_pid.push((
                    row.board_id.to_string(),
                    row.expected_pid.to_string(),
                    got.to_string(),
                ));
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

/// FastLED/fbuild#959 — the six boards whose JSON has null `build.vid`/`build.pid`
/// used to resolve a VID only via the runtime `online-data` `mcu_to_vid` fetch.
/// They must now resolve one **offline** through the compile-time-embedded
/// MCU-family heuristic ([`BoardConfig::resolved_vid`]). No network here.
#[test]
fn issue_959_null_vid_boards_resolve_via_embedded_mcu_heuristic() {
    // (board_id, expected heuristic VID) — the same VIDs the online-data
    // pipeline would have published for each MCU family.
    const NULL_VID_BOARDS: &[(&str, &str)] = &[
        ("esp32-c3-devkitm-1", "303a"),
        ("esp32-c6-devkitc-1", "303a"),
        ("esp32-p4-evboard", "303a"),
        ("esp32doit-devkit-v1", "10c4"),
        ("uno_r4_wifi", "2341"),
        ("lpc845brk", "1fc9"),
    ];

    let mut failures = Vec::new();
    for (board_id, expected_vid) in NULL_VID_BOARDS {
        let config = match BoardConfig::from_board_id(board_id, &HashMap::new()) {
            Ok(c) => c,
            Err(e) => {
                failures.push(format!("{board_id}: failed to load: {e}"));
                continue;
            }
        };
        // Precondition: these boards genuinely lack an explicit build.vid, so
        // the resolution is exercising the embedded heuristic, not JSON.
        if config.vid.is_some() {
            failures.push(format!(
                "{board_id}: now ships an explicit build.vid ({:?}); move it to \
                 FIRST_PARTY_VID_PID_ROWS instead",
                config.vid
            ));
            continue;
        }
        match config.resolved_vid() {
            Some(got) if norm_hex(&got) == norm_hex(expected_vid) => {}
            Some(got) => failures.push(format!(
                "{board_id}: heuristic VID {got} != expected {expected_vid}"
            )),
            None => failures.push(format!(
                "{board_id}: resolved_vid() returned None — online-fallback \
                 regression, the MCU heuristic did not embed/resolve"
            )),
        }
    }

    assert!(
        failures.is_empty(),
        "FastLED/fbuild#959 offline MCU→VID embedding regressed:\n{}",
        failures.join("\n")
    );
}
