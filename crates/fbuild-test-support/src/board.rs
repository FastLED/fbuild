//! Shared board-config helper for unit tests across the workspace.
//!
//! `BoardConfig::from_board_id` is the legacy entry point that
//! `BoardConfig::from_board_id_in_project` superseded when `project_dir`
//! threading landed (#516 / #518). Every production caller routes through
//! `fbuild_build::resolution::resolve_board` now (#519 → #543/#544/#545/
//! #546/#552/#562/#564/#567), but per-test fixtures still need an ergonomic
//! way to materialise a stock `BoardConfig` without each test rebuilding the
//! `(board_id, &HashMap::new())` boilerplate by hand.
//!
//! This helper is the **one** sanctioned test-side call site to
//! `BoardConfig::from_board_id`, completing the #519 acceptance criterion
//! ("≤2 call sites outside `fbuild-config`: the new entry points + one test
//! helper").

use std::collections::HashMap;

use fbuild_config::BoardConfig;

/// Resolve a stock built-in `BoardConfig` for a test fixture (no overrides).
///
/// Panics on lookup failure — tests treat a missing built-in board as a
/// hard configuration error, not a recoverable miss. Pass the canonical
/// `board_id` from the relevant manifest (e.g. `"uno"`, `"teensy41"`,
/// `"rpipico"`, `"lpc845brk"`).
///
/// This is one of the **two** sanctioned test-side callers of
/// `BoardConfig::from_board_id`; the other is
/// [`board_for_test_with_overrides`] for tests that exercise override
/// behaviour. Production code routes through
/// `fbuild_build::resolution::resolve_board` per #519.
#[track_caller]
pub fn board_for_test(board_id: &str) -> BoardConfig {
    board_for_test_with_overrides(board_id, &HashMap::new())
}

/// Resolve a built-in `BoardConfig` for a test fixture, with PlatformIO
/// `[env]` overrides applied. Used by tests that need to verify
/// override-driven behaviour (e.g. `flash_size`, `f_cpu`).
///
/// Same panic semantics as [`board_for_test`].
#[track_caller]
pub fn board_for_test_with_overrides(
    board_id: &str,
    overrides: &HashMap<String, String>,
) -> BoardConfig {
    BoardConfig::from_board_id(board_id, overrides)
        .unwrap_or_else(|e| panic!("BoardConfig should load for built-in board {board_id:?}: {e}"))
}
