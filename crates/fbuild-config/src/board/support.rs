//! Fail-fast check for bundled boards fbuild cannot build
//! (FastLED/fbuild#2039).
//!
//! A board in the bundled database can be unbuildable for two reasons:
//!
//! - its PlatformIO `platform` has no fbuild orchestrator (libretiny,
//!   heltec-cubecell, infineonxmc, ...). Those entries usually also lack the
//!   `build` data a build would need, and nothing could supply it;
//! - the board JSON carries an `unsupported_reason`, because the board needs
//!   something fbuild does not implement at all (e.g. `wasm` needs a
//!   WebAssembly build orchestrator).
//!
//! A buildable board can still be built with the wrong framework: its
//! `frameworks` list names what PlatformIO supports for it, and about a
//! hundred bundled boards on supported platforms are Zephyr-, mbed-,
//! ESP-IDF- or bare-metal-only. PlatformIO's `BuildFrameworks` stops with
//! "This board doesn't support arduino framework!" for those, and so does
//! fbuild when the env names a framework the list lacks (FastLED/fbuild#2042).
//!
//! The build-side board funnel and the daemon handlers call
//! [`ensure_board_buildable`] before resolving packages, so the user gets a
//! typed [`fbuild_core::FbuildError::UnsupportedBoard`] naming the board and
//! the reason instead of a compiler failure deep in the build.

use serde_json::Value;

use super::db::bundled_board_entry;

/// Board JSON key holding the reason fbuild rejects an otherwise-supported
/// board. Survives `enrich_boards` because that tool only rewrites `build`
/// and `upload`.
const UNSUPPORTED_REASON_KEY: &str = "unsupported_reason";

/// Return `Err(UnsupportedBoard)` when `board_id` is a bundled board fbuild
/// cannot build, or one whose `frameworks` list lacks a framework the env's
/// `framework` value (comma-separated, as in `platformio.ini`) requests.
/// Pass `None` when no env is in scope. Unknown and project-local boards
/// pass: their errors come from board resolution.
pub fn ensure_board_buildable(board_id: &str, framework: Option<&str>) -> fbuild_core::Result<()> {
    let Some(entry) = bundled_board_entry(board_id) else {
        return Ok(());
    };
    let reason = unsupported_reason(entry)
        .or_else(|| framework.and_then(|framework| unsupported_framework(entry, framework)));
    let Some(reason) = reason else {
        return Ok(());
    };
    Err(fbuild_core::FbuildError::UnsupportedBoard {
        board: board_id.to_string(),
        reason,
    })
}

/// Why the board described by `entry` cannot build `framework`, if it cannot:
/// a requested name missing from the board's `frameworks` list, compared
/// case-insensitively as fbuild's orchestrators read `framework`. PlatformIO
/// has no framework aliases here; it matches the names as listed. A board
/// JSON with no `frameworks` key is not checked, while an empty list
/// supports no framework, as in PlatformIO.
pub(super) fn unsupported_framework(entry: &Value, framework: &str) -> Option<String> {
    let listed: Vec<&str> = entry
        .get("frameworks")?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let missing: Vec<&str> = framework
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .filter(|name| !listed.iter().any(|l| l.eq_ignore_ascii_case(name)))
        .collect();
    if missing.is_empty() {
        return None;
    }
    let supported = if listed.is_empty() {
        "none".to_string()
    } else {
        listed.join(", ")
    };
    Some(format!(
        "it doesn't support the {} framework (its frameworks: {supported})",
        missing.join(", ")
    ))
}

/// Why fbuild cannot build the board described by `entry`, if it cannot.
pub(super) fn unsupported_reason(entry: &Value) -> Option<String> {
    if let Some(reason) = entry.get(UNSUPPORTED_REASON_KEY).and_then(Value::as_str) {
        return Some(reason.to_string());
    }
    let platform = entry
        .get("platform")
        .and_then(Value::as_str)
        .unwrap_or_default();
    fbuild_core::Platform::from_platform_str(platform)
        .is_none()
        .then(|| format!("its PlatformIO platform '{platform}' has no fbuild build orchestrator"))
}
