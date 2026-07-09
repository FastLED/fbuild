//! Offline MCU-family → USB VID heuristic, embedded at build time.
//!
//! Boards whose PlatformIO definition carries no `build.vid`/`build.pid`
//! (native-USB SoCs like ESP32-C6, LPC845, UNO R4 / RA4M1) previously had a
//! VID only via the `online-data` `mcu_to_vid` pipeline — resolved over the
//! network at runtime. That "online fallback" made offline / air-gapped / CI
//! resolution impossible for those boards (FastLED/fbuild#959, PR #925).
//!
//! This module bakes the exact same curated MCU-family → VID table
//! (`online-data-tools/seed_mcu_to_vid.json`, the seed the online pipeline
//! publishes from) into the binary via `include_str!`, and applies the same
//! matching rule the pipeline's SQLite view uses:
//!
//! ```sql
//! JOIN mcu_to_vid m ON m.mcu_family = b.mcu OR b.mcu LIKE m.mcu_family || '%'
//! ```
//!
//! i.e. a family matches when it equals the board MCU or is a prefix of it,
//! and the highest-`score` match wins. Single source of truth — the same JSON
//! feeds both the online publish and this embedded copy.

use std::sync::OnceLock;

use serde::Deserialize;

/// The curated seed, embedded at compile time (repo-root path so the online
/// publish and the embedded copy never drift).
const SEED_JSON: &str = include_str!("../../../../online-data-tools/seed_mcu_to_vid.json");

#[derive(Deserialize)]
struct SeedRow {
    mcu_family: String,
    vid: String,
    score: f64,
    // `reason` is documentation-only; ignored here.
}

struct McuVid {
    /// Uppercased family key, e.g. `ESP32C6`, `LPC8`, `RA4M1`.
    family_upper: String,
    /// Lowercased 4-hex VID (no `0x`), e.g. `303a`.
    vid: String,
    score: f64,
}

fn table() -> &'static [McuVid] {
    static TABLE: OnceLock<Vec<McuVid>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let rows: Vec<SeedRow> = serde_json::from_str(SEED_JSON)
            .expect("embedded seed_mcu_to_vid.json is valid JSON at build time");
        rows.into_iter()
            .map(|r| McuVid {
                family_upper: r.mcu_family.to_ascii_uppercase(),
                vid: r.vid.to_ascii_lowercase(),
                score: r.score,
            })
            .collect()
    })
}

/// Best-guess USB VID (lowercase 4-hex, no `0x`) for an MCU string, using the
/// same `family == mcu OR mcu LIKE family%` + highest-score rule as the
/// online `mcu_to_vid` pipeline. `None` when no family matches.
///
/// Purely a resolution/display heuristic — it deliberately does NOT feed the
/// `-DUSB_VID` compile define (that stays keyed on an explicit `build.vid`),
/// so adding this cannot change any board's compiled output.
pub fn vid_for_mcu(mcu: &str) -> Option<&'static str> {
    if mcu.is_empty() {
        return None;
    }
    let mcu_upper = mcu.to_ascii_uppercase();
    table()
        .iter()
        .filter(|r| mcu_upper == r.family_upper || mcu_upper.starts_with(&r.family_upper))
        .max_by(|a, b| {
            a.score
                .partial_cmp(&b.score)
                // Tie-break deterministically on the longer (more specific)
                // family so `ESP32C6` beats a same-score `ESP32` prefix.
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.family_upper.len().cmp(&b.family_upper.len()))
        })
        .map(|r| r.vid.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_seed_is_nonempty() {
        assert!(!table().is_empty(), "embedded mcu_to_vid seed should parse");
    }

    #[test]
    fn native_usb_socs_resolve_a_vid_offline() {
        // The families behind the six null-vid boards #959 targets.
        assert_eq!(vid_for_mcu("ESP32C6"), Some("303a"));
        assert_eq!(vid_for_mcu("ESP32C3"), Some("303a"));
        assert_eq!(vid_for_mcu("ESP32P4"), Some("303a"));
        assert_eq!(vid_for_mcu("RA4M1"), Some("2341"));
        // LPC845 matches the `LPC8` family prefix.
        assert_eq!(vid_for_mcu("LPC845"), Some("1fc9"));
        // Plain ESP32 (esp32doit-devkit-v1) → CP210x bridge.
        assert_eq!(vid_for_mcu("ESP32"), Some("10c4"));
    }

    #[test]
    fn prefix_specificity_beats_shorter_family() {
        // `ESP32C6` (0.90) must win over the `ESP32` (0.70) prefix match.
        assert_eq!(vid_for_mcu("ESP32C6"), Some("303a"));
        // Case-insensitive.
        assert_eq!(vid_for_mcu("esp32c6"), Some("303a"));
    }

    #[test]
    fn unknown_mcu_returns_none() {
        assert_eq!(vid_for_mcu("TOTALLY_FAKE_MCU"), None);
        assert_eq!(vid_for_mcu(""), None);
    }
}
