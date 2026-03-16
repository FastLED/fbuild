//! One-off maintenance tool — not part of the build or CI pipeline.
//!
//! Enriches stripped board JSONs with `build` and `upload` sections from
//! PlatformIO's full board definitions (~/.platformio/platforms/).
//!
//! Run this manually when board definitions need updating (e.g. after adding
//! new boards or upgrading PlatformIO platform packages). The enriched JSONs
//! are committed to the repo and used as static assets at compile time.
//!
//! Usage:
//!     uv run cargo run -p fbuild-config --bin enrich_boards

use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const BUILD_FIELDS: &[&str] = &[
    "core",
    "variant",
    "extra_flags",
    "f_cpu",
    "f_flash",
    "flash_mode",
    "mcu",
];

const ARDUINO_FIELDS: &[&str] = &["ldscript", "partitions", "memory_type"];

const UPLOAD_FIELDS: &[&str] = &["protocol", "speed", "flash_size", "require_upload_port"];

fn home_dir() -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(std::env::var("USERPROFILE").expect("USERPROFILE not set"))
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(std::env::var("HOME").expect("HOME not set"))
    }
}

fn pio_platforms_dir() -> PathBuf {
    home_dir().join(".platformio").join("platforms")
}

fn boards_dir() -> PathBuf {
    // Relative to workspace root
    PathBuf::from("crates/fbuild-config/assets/boards/json")
}

/// Find the full PlatformIO board JSON for a given board_id and platform.
fn find_pio_board(board_id: &str, platform: &str, pio_dir: &Path) -> Option<Value> {
    // Try the base platform directory (active version)
    let board_path = pio_dir
        .join(platform)
        .join("boards")
        .join(format!("{board_id}.json"));
    if board_path.exists() {
        let contents = fs::read_to_string(&board_path).ok()?;
        return serde_json::from_str(&contents).ok();
    }

    // Try versioned platform directories (espressif32@src-xxx)
    if pio_dir.exists() {
        let prefix = format!("{platform}@");
        if let Ok(entries) = fs::read_dir(pio_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with(&prefix) && entry.path().is_dir() {
                    let board_path = entry.path().join("boards").join(format!("{board_id}.json"));
                    if board_path.exists() {
                        let contents = fs::read_to_string(&board_path).ok()?;
                        return serde_json::from_str(&contents).ok();
                    }
                }
            }
        }
    }

    None
}

/// Normalize extra_flags to a space-separated string.
fn normalize_extra_flags(val: &Value) -> Value {
    match val {
        Value::Array(arr) => {
            let joined: String = arr
                .iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            Value::String(joined)
        }
        Value::String(_) => val.clone(),
        _ => Value::String(String::new()),
    }
}

/// Extract relevant build fields from PlatformIO's build section.
fn extract_build(pio_build: &Map<String, Value>) -> Map<String, Value> {
    let mut build = Map::new();

    for &field in BUILD_FIELDS {
        if let Some(val) = pio_build.get(field) {
            let val = if field == "extra_flags" {
                normalize_extra_flags(val)
            } else {
                val.clone()
            };
            build.insert(field.to_string(), val);
        }
    }

    // Extract arduino sub-fields
    if let Some(Value::Object(arduino_src)) = pio_build.get("arduino") {
        let mut arduino = Map::new();
        for &field in ARDUINO_FIELDS {
            if let Some(val) = arduino_src.get(field) {
                arduino.insert(field.to_string(), val.clone());
            }
        }
        if !arduino.is_empty() {
            build.insert("arduino".to_string(), Value::Object(arduino));
        }
    }

    build
}

/// Extract relevant upload fields from PlatformIO's upload section.
fn extract_upload(pio_upload: &Map<String, Value>) -> Map<String, Value> {
    let mut upload = Map::new();
    for &field in UPLOAD_FIELDS {
        if let Some(val) = pio_upload.get(field) {
            upload.insert(field.to_string(), val.clone());
        }
    }
    upload
}

/// Enrich a single board JSON. Returns true if the file was modified.
fn enrich_board(board_path: &Path, pio_dir: &Path) -> Result<bool, String> {
    let contents = fs::read_to_string(board_path)
        .map_err(|e| format!("read {}: {e}", board_path.display()))?;
    let mut board: Value = serde_json::from_str(&contents)
        .map_err(|e| format!("parse {}: {e}", board_path.display()))?;

    let obj = board.as_object().ok_or("board JSON is not an object")?;
    let board_id = obj
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| board_path.file_stem().unwrap().to_str().unwrap());
    let platform = obj.get("platform").and_then(|v| v.as_str()).unwrap_or("");

    if platform.is_empty() {
        return Ok(false);
    }

    let pio_board = match find_pio_board(board_id, platform, pio_dir) {
        Some(b) => b,
        None => return Ok(false),
    };

    let pio_obj = pio_board
        .as_object()
        .ok_or("PIO board JSON is not an object")?;
    let board_obj = board.as_object_mut().ok_or("board JSON is not an object")?;
    let mut changed = false;

    // Extract and merge build section
    if let Some(Value::Object(pio_build)) = pio_obj.get("build") {
        let build = extract_build(pio_build);
        if !build.is_empty() {
            board_obj.insert("build".to_string(), Value::Object(build));
            changed = true;
        }
    }

    // Extract and merge upload section
    if let Some(Value::Object(pio_upload)) = pio_obj.get("upload") {
        let upload = extract_upload(pio_upload);
        if !upload.is_empty() {
            board_obj.insert("upload".to_string(), Value::Object(upload));
            changed = true;
        }
    }

    if changed {
        let json = serde_json::to_string_pretty(&board)
            .map_err(|e| format!("serialize {}: {e}", board_path.display()))?;
        fs::write(board_path, format!("{json}\n"))
            .map_err(|e| format!("write {}: {e}", board_path.display()))?;
    }

    Ok(changed)
}

fn main() {
    let boards_dir = boards_dir();
    let pio_dir = pio_platforms_dir();

    if !boards_dir.exists() {
        eprintln!("Error: {} not found", boards_dir.display());
        std::process::exit(1);
    }

    if !pio_dir.exists() {
        eprintln!(
            "Warning: {} not found, no enrichment possible",
            pio_dir.display()
        );
        std::process::exit(1);
    }

    let mut board_files: Vec<PathBuf> = fs::read_dir(&boards_dir)
        .expect("failed to read boards directory")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .collect();
    board_files.sort();

    let total = board_files.len();
    let mut enriched = 0u32;
    let mut skipped = 0u32;
    let mut errors = 0u32;

    for board_path in &board_files {
        match enrich_board(board_path, &pio_dir) {
            Ok(true) => enriched += 1,
            Ok(false) => skipped += 1,
            Err(e) => {
                eprintln!(
                    "  Error enriching {}: {e}",
                    board_path.file_stem().unwrap().to_string_lossy()
                );
                errors += 1;
            }
        }
    }

    println!("Enrichment complete:");
    println!("  Total:    {total}");
    println!("  Enriched: {enriched}");
    println!("  Skipped:  {skipped}");
    println!("  Errors:   {errors}");

    // Report which platforms were found
    let mut platforms_found = BTreeSet::new();
    let mut platforms_missing = BTreeSet::new();

    for board_path in &board_files {
        if let Ok(contents) = fs::read_to_string(board_path) {
            if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(&contents) {
                if let Some(platform) = obj.get("platform").and_then(|v| v.as_str()) {
                    if !platform.is_empty() {
                        if pio_dir.join(platform).exists() {
                            platforms_found.insert(platform.to_string());
                        } else {
                            platforms_missing.insert(platform.to_string());
                        }
                    }
                }
            }
        }
    }

    if !platforms_found.is_empty() {
        let list: Vec<&str> = platforms_found.iter().map(|s| s.as_str()).collect();
        println!("\n  Platforms with local installs: {}", list.join(", "));
    }
    if !platforms_missing.is_empty() {
        let list: Vec<&str> = platforms_missing.iter().map(|s| s.as_str()).collect();
        println!("  Platforms without local installs: {}", list.join(", "));
    }
}
