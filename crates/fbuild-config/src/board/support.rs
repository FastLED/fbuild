//! Fail-fast check for bundled boards fbuild cannot build
//! (FastLED/fbuild#2039).
//!
//! A board in the bundled database can be unbuildable for two reasons:
//!
//! - its PlatformIO `platform` has no fbuild orchestrator (libretiny,
//!   heltec-cubecell, infineonxmc, ...). Those entries usually also lack the
//!   `build` data a build would need, and nothing could supply it;
//! - the board JSON carries an `unsupported_reason`, because the board needs
//!   something fbuild does not install yet (e.g. `portenta_c33` needs the
//!   `renesas_portenta` core).
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
/// cannot build. Unknown and project-local boards pass: their errors come
/// from board resolution.
pub fn ensure_board_buildable(board_id: &str) -> fbuild_core::Result<()> {
    let Some(reason) = bundled_board_entry(board_id).and_then(unsupported_reason) else {
        return Ok(());
    };
    Err(fbuild_core::FbuildError::UnsupportedBoard {
        board: board_id.to_string(),
        reason,
    })
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
