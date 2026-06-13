//! Tests for project-local `boards/<id>.json` resolution (FastLED/fbuild#515).
//!
//! Verifies that `BoardConfig::from_board_id_in_project` accepts a
//! PlatformIO-style project-local board manifest as a fallback when the
//! built-in board database has no entry for the requested board id, and
//! that the function still behaves identically to `from_board_id` when
//! no project_dir is provided.

use std::collections::HashMap;
use std::io::Write;

use tempfile::TempDir;

use super::BoardConfig;

/// Write a JSON file at `<dir>/boards/<id>.json` and return the directory.
fn write_project_board(dir: &TempDir, board_id: &str, json: &str) {
    let boards_dir = dir.path().join("boards");
    std::fs::create_dir_all(&boards_dir).unwrap();
    let path = boards_dir.join(format!("{}.json", board_id));
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(json.as_bytes()).unwrap();
    f.flush().unwrap();
}

const LPC845BRK_PIO_JSON: &str = r#"{
  "build": {
    "core": "lpc8xx",
    "cpu": "cortex-m0plus",
    "extra_flags": "-DCPU_LPC845M301JBD48 -D__LPC845__ -DLPC845 -DARDUINO_LPC845BRK",
    "f_cpu": "30000000L",
    "mcu": "lpc845",
    "variant": "lpc845brk"
  },
  "frameworks": ["arduino"],
  "name": "NXP LPC845-BRK",
  "upload": {
    "maximum_ram_size": 16384,
    "maximum_size": 65536,
    "protocol": "cmsis-dap"
  },
  "vendor": "NXP"
}"#;

#[test]
fn project_local_board_resolves_when_bundled_db_misses() {
    let dir = TempDir::new().unwrap();
    write_project_board(&dir, "lpc845brk-test", LPC845BRK_PIO_JSON);

    let cfg =
        BoardConfig::from_board_id_in_project("lpc845brk-test", &HashMap::new(), Some(dir.path()))
            .expect("project-local board should resolve");

    assert_eq!(cfg.name, "NXP LPC845-BRK");
    assert_eq!(cfg.mcu, "lpc845");
    assert_eq!(cfg.f_cpu, "30000000L");
    assert_eq!(cfg.core, "lpc8xx");
    assert_eq!(cfg.variant, "lpc845brk");
    assert_eq!(cfg.max_flash, Some(65_536));
    assert_eq!(cfg.max_ram, Some(16_384));
    assert_eq!(cfg.upload_protocol.as_deref(), Some("cmsis-dap"));
    // Board-level extra_flags carry the Arduino board macro.
    let defines = cfg.get_defines();
    assert_eq!(defines.get("ARDUINO_LPC845BRK"), Some(&"1".to_string()));
    assert_eq!(defines.get("CPU_LPC845M301JBD48"), Some(&"1".to_string()));
}

#[test]
fn project_local_board_not_consulted_without_project_dir() {
    // With no project_dir, the lookup must fall straight through to the
    // bundled DB; an unknown id stays unknown even if a file would have
    // satisfied it.
    let dir = TempDir::new().unwrap();
    write_project_board(&dir, "lpc845brk-test", LPC845BRK_PIO_JSON);

    let result = BoardConfig::from_board_id_in_project("lpc845brk-test", &HashMap::new(), None);
    assert!(
        result.is_err(),
        "expected unknown-board error when project_dir is None"
    );
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("unknown board"), "msg was: {}", msg);
}

#[test]
fn bundled_board_wins_over_project_local() {
    // 'uno' is a well-known bundled board. A project-local file with a
    // bogus mcu must NOT override the bundled defaults — project-local is
    // a fallback only.
    let dir = TempDir::new().unwrap();
    let bogus_uno = r#"{
        "build": {"mcu": "definitely-not-atmega328p", "core": "arduino", "variant": "standard"},
        "name": "Project-Local Uno (should be ignored)"
    }"#;
    write_project_board(&dir, "uno", bogus_uno);

    let cfg = BoardConfig::from_board_id_in_project("uno", &HashMap::new(), Some(dir.path()))
        .expect("bundled 'uno' must resolve");
    assert_eq!(
        cfg.mcu, "atmega328p",
        "bundled board defaults should win over project-local"
    );
}

#[test]
fn project_local_missing_file_returns_unknown_board() {
    let dir = TempDir::new().unwrap();
    // No boards/ directory created.

    let result =
        BoardConfig::from_board_id_in_project("lpc845brk-test", &HashMap::new(), Some(dir.path()));
    assert!(result.is_err(), "missing file should yield unknown-board");
}

#[test]
fn project_local_unparseable_file_returns_unknown_board() {
    let dir = TempDir::new().unwrap();
    write_project_board(&dir, "bad-json", "{ this is not json");

    let result =
        BoardConfig::from_board_id_in_project("bad-json", &HashMap::new(), Some(dir.path()));
    assert!(
        result.is_err(),
        "malformed JSON should yield unknown-board (logged as a warning)"
    );
}

#[test]
fn from_board_id_is_equivalent_to_in_project_with_none() {
    let a = BoardConfig::from_board_id("uno", &HashMap::new()).unwrap();
    let b = BoardConfig::from_board_id_in_project("uno", &HashMap::new(), None).unwrap();
    assert_eq!(a.name, b.name);
    assert_eq!(a.mcu, b.mcu);
    assert_eq!(a.f_cpu, b.f_cpu);
    assert_eq!(a.board, b.board);
    assert_eq!(a.core, b.core);
    assert_eq!(a.variant, b.variant);
    assert_eq!(a.max_flash, b.max_flash);
    assert_eq!(a.max_ram, b.max_ram);
}

/// A board id absent from the built-in DB but present as
/// `<project>/boards/<id>.json` must resolve via the project_dir path
/// instead of silently falling back to the platform default (#519).
#[test]
fn from_board_id_or_default_resolves_project_local_board() {
    let dir = tempfile::tempdir().unwrap();
    write_project_board(
        &dir,
        "widget123",
        r#"{"mcu":"atmega328p","f_cpu":"16000000L","core":"arduino","variant":"standard"}"#,
    );
    let config = BoardConfig::from_board_id_or_default(
        "widget123",
        "uno",
        &HashMap::new(),
        Some(dir.path()),
    );
    assert!(
        config.board.eq_ignore_ascii_case("widget123"),
        "expected project-local board, got '{}'",
        config.board
    );
}

#[test]
fn from_board_id_with_override_fallback_resolves_project_local_board() {
    let dir = tempfile::tempdir().unwrap();
    write_project_board(
        &dir,
        "widget456",
        r#"{"mcu":"atmega328p","f_cpu":"16000000L","core":"arduino","variant":"standard"}"#,
    );
    let board = BoardConfig::from_board_id_with_override_fallback(
        "widget456",
        &HashMap::new(),
        Some(dir.path()),
    )
    .expect("project-local board must resolve");
    assert!(
        board.board.eq_ignore_ascii_case("widget456"),
        "expected project-local board, got '{}'",
        board.board
    );
}
