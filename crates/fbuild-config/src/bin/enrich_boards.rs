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
//!     soldr cargo run -p fbuild-config --bin enrich_boards

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
    "f_image",
    "flash_mode",
    "ldscript",
    "mcu",
    "openocd_target",
    // USB product display string (drives USB_PRODUCT/USB_MANUFACTURER for
    // USB-native cores). NOT a USB identity: `vid`/`pid`/`hwids` stay banned
    // from bundled snapshots (see the explicit removal below and the
    // `bundled_board_snapshots_never_embed_usb_vid_or_pid` guard).
    "usb_product",
];

const ARDUINO_FIELDS: &[&str] = &["ldscript", "partitions", "memory_type"];

const UPLOAD_FIELDS: &[&str] = &[
    "protocol",
    "speed",
    "flash_size",
    "require_upload_port",
    "maximum_ram_size",
    "maximum_size",
    "use_1200bps_touch",
    "wait_for_upload_port",
];

const MEGATINYCORE_EXTRA_FLAGS: &[&str] = &[
    "-DCLOCK_SOURCE=0",
    r#"-DMEGATINYCORE="2.6.11""#,
    "-DMEGATINYCORE_MAJOR=2UL",
    "-DMEGATINYCORE_MINOR=6UL",
    "-DMEGATINYCORE_PATCH=11UL",
    "-DMEGATINYCORE_RELEASED=1",
    "-DCORE_ATTACH_ALL",
    "-DTWI_MORS",
    "-DUSE_TIMERD0_PWM",
];

const DXCORE_EXTRA_FLAGS: &[&str] = &[
    "-DCLOCK_SOURCE=0",
    r#"-DDXCORE="1.5.6""#,
    "-DDXCORE_MAJOR=1UL",
    "-DDXCORE_MINOR=5UL",
    "-DDXCORE_PATCH=6UL",
    "-DDXCORE_RELEASED=1",
    "-DCORE_ATTACH_ALL",
    "-DTWI_MORS_SINGLE",
    "-DMILLIS_USE_TIMERB2",
];

fn home_dir() -> PathBuf {
    fbuild_core::platform::host::home_dir()
        .expect("home directory not set")
        .into_path_buf()
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

fn framework_extra_flags(core: Option<&str>) -> &'static [&'static str] {
    match core {
        Some("megatinycore") => MEGATINYCORE_EXTRA_FLAGS,
        Some("dxcore") => DXCORE_EXTRA_FLAGS,
        _ => &[],
    }
}

/// Join a separated `-D NAME` into `-DNAME`. Some upstream PlatformIO boards
/// (`pico`, `nanorp2040connect`) use the spaced form; the bundle keeps one
/// token per define (FastLED/fbuild#2038). Whitespace-token level on purpose:
/// `ini_parser::parse_flags` does the same join but also strips the quoting
/// layer, which the stored JSON must keep.
fn join_separated_defines(flags: &str) -> Vec<String> {
    let mut tokens = flags.split_whitespace();
    let mut joined = Vec::new();
    while let Some(token) = tokens.next() {
        match (token, tokens.clone().next()) {
            ("-D", Some(name)) => {
                tokens.next();
                joined.push(format!("-D{name}"));
            }
            _ => joined.push(token.to_string()),
        }
    }
    joined
}

fn merge_extra_flags(core: Option<&str>, flags: &str) -> String {
    let mut merged = join_separated_defines(flags);
    let existing: BTreeSet<String> = merged.iter().cloned().collect();

    // PlatformIO injects these framework defines at build time rather than
    // storing them in the board JSON, but fbuild consumes the static board
    // assets directly and needs the full define set preserved there.
    for flag in framework_extra_flags(core) {
        if !existing.contains(*flag) {
            merged.push((*flag).to_string());
        }
    }

    merged.join(" ")
}

/// Identity of a flag for de-duplication: the macro name for `-DNAME[=v]`,
/// the whole token otherwise.
fn flag_key(flag: &str) -> &str {
    match flag.strip_prefix("-D") {
        Some(define) => &flag[..2 + define.find('=').unwrap_or(define.len())],
        None => flag,
    }
}

/// Merge regenerated upstream `extra_flags` into the bundled ones instead of
/// replacing them, so defines fbuild added by hand survive a re-run
/// (FastLED/fbuild#2039: `uno_r4_wifi`'s `ARDUINO_FSP`, `_RA_CORE`, ...).
///
/// Upstream flags come first, in upstream order, and win when both define
/// the same macro; bundled-only flags follow in their bundled order;
/// duplicates (by macro name) are dropped. A board with no hand-added flags
/// therefore regenerates to exactly the upstream string, which is what
/// `ci/validate_boards.py` compares against.
fn merge_with_bundled_flags(bundled: &str, upstream: &str) -> String {
    let mut seen = BTreeSet::new();
    let mut merged = Vec::new();
    for flag in join_separated_defines(upstream)
        .into_iter()
        .chain(join_separated_defines(bundled))
    {
        if seen.insert(flag_key(&flag).to_string()) {
            merged.push(flag);
        }
    }
    merged.join(" ")
}

/// Fold the bundled `extra_flags` of `existing` into freshly extracted
/// upstream `build` fields before they are merged over `existing`.
fn keep_bundled_extra_flags(existing: &Map<String, Value>, build: &mut Map<String, Value>) {
    let bundled = existing.get("extra_flags").and_then(Value::as_str);
    let upstream = build.get("extra_flags").and_then(Value::as_str);
    if let (Some(bundled), Some(upstream)) = (bundled, upstream) {
        let merged = merge_with_bundled_flags(bundled, upstream);
        build.insert("extra_flags".to_string(), Value::String(merged));
    }
}

/// Extract relevant build fields from PlatformIO's build section.
fn extract_build(pio_build: &Map<String, Value>) -> Map<String, Value> {
    let mut build = Map::new();
    let core = pio_build.get("core").and_then(|v| v.as_str());

    for &field in BUILD_FIELDS {
        if let Some(val) = pio_build.get(field) {
            let val = if field == "extra_flags" {
                let normalized = normalize_extra_flags(val);
                Value::String(merge_extra_flags(
                    core,
                    normalized.as_str().unwrap_or_default(),
                ))
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

/// Merge fields from `src` into `dst`, recursively merging nested objects so
/// existing fbuild-only keys are preserved. PIO-supplied values overwrite
/// any pre-existing value with the same key.
fn merge_into(dst: &mut Map<String, Value>, src: Map<String, Value>) {
    for (k, v) in src {
        match (dst.get_mut(&k), v) {
            (Some(Value::Object(existing)), Value::Object(new_obj)) => {
                merge_into(existing, new_obj);
            }
            (_, new_val) => {
                dst.insert(k, new_val);
            }
        }
    }
}

/// Enrich a single board JSON. Returns true if the file was modified.
fn enrich_board(board_path: &Path, pio_dir: &Path) -> Result<bool, String> {
    let contents = fs::read_to_string(board_path)
        .map_err(|e| format!("read {}: {e}", board_path.display()))?;
    let mut board: Value = serde_json::from_str(&contents)
        .map_err(|e| format!("parse {}: {e}", board_path.display()))?;

    let obj = board.as_object().ok_or("board JSON is not an object")?;
    let board_id = obj.get("id").and_then(|v| v.as_str()).unwrap_or_else(|| {
        board_path
            .file_stem()
            .expect("fbuild-config: board JSON path always has a file stem")
            .to_str()
            .expect("fbuild-config: board JSON file stems are ASCII")
    });
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

    // Merge build section. We MERGE rather than replace so existing fbuild
    // fields that PIO doesn't ship (e.g. legacy `arduino.ldscript` that
    // PlatformIO has since moved into its SCons builder scripts) survive a
    // re-run of this tool. PIO-supplied fields still override existing values,
    // except `extra_flags`, which keeps hand-added defines (FastLED/fbuild#2039).
    if let Some(Value::Object(pio_build)) = pio_obj.get("build") {
        let mut build = extract_build(pio_build);
        if !build.is_empty() {
            let existing = board_obj
                .entry("build".to_string())
                .or_insert_with(|| Value::Object(Map::new()));
            if let Value::Object(existing_build) = existing {
                keep_bundled_extra_flags(existing_build, &mut build);
                // USB identity belongs exclusively to the published
                // FastLED/boards registry, never this bundled snapshot.
                existing_build.remove("vid");
                existing_build.remove("pid");
                merge_into(existing_build, build);
                changed = true;
            }
        }
    }

    // Merge upload section (same rationale as build).
    if let Some(Value::Object(pio_upload)) = pio_obj.get("upload") {
        let upload = extract_upload(pio_upload);
        if !upload.is_empty() {
            let existing = board_obj
                .entry("upload".to_string())
                .or_insert_with(|| Value::Object(Map::new()));
            if let Value::Object(existing_upload) = existing {
                merge_into(existing_upload, upload);
                changed = true;
            }
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
                    board_path
                        .file_stem()
                        .expect("fbuild-config: board JSON path always has a file stem")
                        .to_string_lossy()
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

    report_arduino_boards_without_variant(&board_files);
}

/// True when a board targets the Arduino framework but its JSON names no
/// `build.variant` (or has no `build` block at all).
fn arduino_board_lacks_variant(board: &Value) -> bool {
    let is_arduino = board
        .get("frameworks")
        .and_then(Value::as_array)
        .is_some_and(|fws| fws.iter().any(|fw| fw.as_str() == Some("arduino")));
    let has_variant = board
        .get("build")
        .and_then(|build| build.get("variant"))
        .and_then(Value::as_str)
        .is_some_and(|variant| !variant.is_empty());
    is_arduino && !has_variant
}

/// Loudly list Arduino boards that are still variant-less after enrichment.
///
/// A board this tool cannot enrich (its platform is not installed under
/// `~/.platformio/platforms`, or PlatformIO ships no JSON for it) keeps its
/// stripped form and later resolves to a fallback variant. For cores with
/// per-board variants that drops the variant's include path entirely —
/// `uno_r4_minima` shipped that way and failed at `bsp_api.h`
/// (FastLED/fbuild#2015). Some cores (Teensy, ESP32) legitimately need no
/// variant, so this is a report for the maintainer, not a hard failure.
fn report_arduino_boards_without_variant(board_files: &[PathBuf]) {
    let mut by_platform: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for board_path in board_files {
        let Ok(contents) = fs::read_to_string(board_path) else {
            continue;
        };
        let Ok(board) = serde_json::from_str::<Value>(&contents) else {
            continue;
        };
        if !arduino_board_lacks_variant(&board) {
            continue;
        }
        let platform = board.get("platform").and_then(Value::as_str).unwrap_or("?");
        let id = board_path.file_stem().unwrap_or_default().to_string_lossy();
        by_platform
            .entry(platform.to_string())
            .or_default()
            .push(id.into_owned());
    }
    if by_platform.is_empty() {
        return;
    }
    let total: usize = by_platform.values().map(Vec::len).sum();
    eprintln!(
        "\nWarning: {total} arduino board(s) have no build.variant (fallback variant at build time):"
    );
    for (platform, ids) in &by_platform {
        eprintln!("  {platform}: {}", ids.join(", "));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FastLED/fbuild#2038: upstream `pico.json` writes `-D ARDUINO_...`;
    /// regenerating must not reintroduce the spaced form into the bundle.
    #[test]
    fn merge_extra_flags_joins_separated_defines() {
        assert_eq!(
            merge_extra_flags(None, "-D ARDUINO_RASPBERRY_PI_PICO -DARDUINO_ARCH_RP2040"),
            "-DARDUINO_RASPBERRY_PI_PICO -DARDUINO_ARCH_RP2040"
        );
        assert_eq!(merge_extra_flags(None, "-DA -D"), "-DA -D");
    }

    /// FastLED/fbuild#2039: regenerating `uno_r4_wifi` from upstream
    /// platform-renesas-ra must not drop the defines fbuild added by hand.
    #[test]
    fn regenerating_keeps_hand_added_extra_flags() {
        let bundled = "-DF_CPU=48000000L -DARDUINO_UNOR4_WIFI -DARDUINO_UNOWIFIR4 -DNO_USB \
                       -DBACKTRACE_SUPPORT -DARDUINO_ARCH_RENESAS -DARDUINO_ARCH_RENESAS_UNO \
                       -DARDUINO_FSP -D_XOPEN_SOURCE=700 -DCFG_TUSB_MCU=OPT_MCU_RAXXX \
                       -D_RA_CORE=CM4 -D_RENESAS_RA_";
        let pio_build: Map<String, Value> = serde_json::from_value(serde_json::json!({
            "core": "arduino",
            "variant": "UNOWIFIR4",
            "extra_flags": [
                "-DARDUINO_UNOWIFIR4", "-DARDUINO_ARCH_RENESAS_UNO", "-DARDUINO_UNOR4_WIFI",
                "-DNO_USB", "-DBACKTRACE_SUPPORT", "-DLWIP_DNS=1", "-DCFG_TUSB_MCU=OPT_MCU_RAXXX"
            ]
        }))
        .unwrap();
        let mut existing = Map::new();
        existing.insert("extra_flags".into(), Value::String(bundled.into()));
        let mut build = extract_build(&pio_build);
        keep_bundled_extra_flags(&existing, &mut build);
        merge_into(&mut existing, build);
        assert_eq!(
            existing["extra_flags"].as_str().unwrap(),
            "-DARDUINO_UNOWIFIR4 -DARDUINO_ARCH_RENESAS_UNO -DARDUINO_UNOR4_WIFI -DNO_USB \
             -DBACKTRACE_SUPPORT -DLWIP_DNS=1 -DCFG_TUSB_MCU=OPT_MCU_RAXXX \
             -DF_CPU=48000000L -DARDUINO_ARCH_RENESAS -DARDUINO_FSP -D_XOPEN_SOURCE=700 \
             -D_RA_CORE=CM4 -D_RENESAS_RA_"
        );
    }

    #[test]
    fn merge_with_bundled_flags_lets_upstream_values_win_and_dedupes() {
        assert_eq!(
            merge_with_bundled_flags("-DX=1 -DHAND -D Y -mthumb", "-DX=2 -DY -DUP"),
            "-DX=2 -DY -DUP -DHAND -mthumb"
        );
        // No hand-added flags: the regenerated string is exactly upstream's.
        assert_eq!(merge_with_bundled_flags("-DB -DA", "-DA -DB"), "-DA -DB");
    }

    #[test]
    fn keep_bundled_extra_flags_needs_both_sides() {
        let mut build = Map::new();
        build.insert("core".into(), Value::String("arduino".into()));
        let mut existing = Map::new();
        existing.insert("extra_flags".into(), Value::String("-DHAND".into()));
        keep_bundled_extra_flags(&existing, &mut build);
        assert!(!build.contains_key("extra_flags"));
        merge_into(&mut existing, build);
        assert_eq!(existing["extra_flags"], "-DHAND");
    }

    #[test]
    fn merge_extra_flags_keeps_escaped_quotes_and_dedupes_framework_flags() {
        assert_eq!(
            merge_extra_flags(None, r#"-DUSB_PRODUCT=\"NanoESP32\""#),
            r#"-DUSB_PRODUCT=\"NanoESP32\""#
        );
        let merged = merge_extra_flags(Some("megatinycore"), "-D CLOCK_SOURCE=0");
        assert_eq!(merged.matches("-DCLOCK_SOURCE=0").count(), 1, "{merged}");
    }
}
