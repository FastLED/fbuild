//! Regression tests for the FastLED/boards USB identity boundary.

use std::fs;
use std::path::PathBuf;

use serde_json::Value;

fn bundled_boards_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/boards/json")
}

#[test]
fn bundled_board_snapshots_never_embed_usb_vid_or_pid() {
    let mut violations = Vec::new();
    for entry in fs::read_dir(bundled_boards_dir()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let Some(build) = value.get("build").and_then(Value::as_object) else {
            continue;
        };
        for field in ["vid", "pid"] {
            if build.contains_key(field) {
                violations.push(format!("{}: build.{field}", path.display()));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "bundled USB identities must be published by FastLED/boards, not copied into fbuild:\n{}",
        violations.join("\n")
    );
}
