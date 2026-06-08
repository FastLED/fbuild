//! `build_info_<env>.json` shrink telemetry (FastLED/fbuild#493, #506).
//!
//! Phase 1e (this file) defines the [`ShrinkRecord`] and [`Strategy`] types
//! that will eventually be embedded in `build_info_<env>.json` so
//! `fbuild bloat` and CI bloat-budget gates can label per-symbol reports
//! with the shrink decision that produced them.
//!
//! Phase 1e does *not* wire the record into the existing
//! [`crate::build_info::BuildInfo`] emitter — that hookup lands when an
//! orchestrator first calls
//! [`super::resolver::resolve_for_context`] end-to-end (Phase 4). This file
//! is just the schema + round-trip tests.

use serde::{Deserialize, Serialize};

use super::ShrinkMode;

/// Which linking strategy the shrink pipeline picked.
///
/// `SpecFile` is the primary path (per #493): a GCC `--specs=` file that
/// pulls `libprintf_thin.a` before `-lc`. `Wrap` is the fallback used by
/// `--shrink=printf` for single-knob A/B debugging via `-Wl,--wrap=`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Strategy {
    /// Primary path: `--specs=printf-thin.specs` + shadow archive.
    SpecFile,
    /// Fallback path: per-symbol `-Wl,--wrap=` redirections plus a
    /// `noinline,externally_visible`-marked stub TU. Used only when
    /// `--shrink=printf` is selected explicitly for debugging.
    Wrap,
}

/// Per-build shrink decision recorded into `build_info_<env>.json`.
///
/// All fields are populated from the same data the green one-liner and the
/// verbose plan use, so reports stay in sync with the JSON.
///
/// JSON layout (kebab-case, omits `strategy` when `None`):
///
/// ```json
/// {
///   "mode": "auto",
///   "resolved": "safe",
///   "applied": ["printf-thin", "scanf-thin"],
///   "strategy": "spec-file"
/// }
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShrinkRecord {
    /// Explicit mode after the CLI-level `resolve_explicit`. `Auto` when
    /// the user didn't pass `--shrink=…` or `--no-shrink`.
    pub mode: ShrinkMode,
    /// Per-platform mode after `resolve_for_context`. Equals `mode` for
    /// every explicit mode; for `Auto` it is whatever the platform's
    /// auto-resolver picked. In Phase 1c the auto-resolver always returns
    /// `Off`, so this field is currently always `Off` whenever `mode` is
    /// `Auto`.
    pub resolved: ShrinkMode,
    /// Shrinker categories that fired during this build (e.g.
    /// `"printf-thin"`, `"scanf-thin"`, `"esp-err-msg-strip"`).
    #[serde(default)]
    pub applied: Vec<String>,
    /// Link strategy fbuild used; `None` when `resolved == Off` or when
    /// no platform-specific shrinker fired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<Strategy>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip<T>(value: &T) -> T
    where
        T: Serialize + for<'de> Deserialize<'de>,
    {
        let json = serde_json::to_string(value).expect("serialize");
        serde_json::from_str(&json).expect("deserialize")
    }

    #[test]
    fn shrink_mode_kebab_case_round_trips() {
        for mode in [
            ShrinkMode::Auto,
            ShrinkMode::Off,
            ShrinkMode::Safe,
            ShrinkMode::Aggressive,
            ShrinkMode::Printf,
        ] {
            let json = serde_json::to_string(&mode).expect("serialize");
            let decoded: ShrinkMode = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(decoded, mode, "round-trip mismatch for {mode:?}");
        }
    }

    #[test]
    fn shrink_mode_serializes_as_kebab_case_string() {
        assert_eq!(
            serde_json::to_string(&ShrinkMode::Auto).unwrap(),
            "\"auto\""
        );
        assert_eq!(serde_json::to_string(&ShrinkMode::Off).unwrap(), "\"off\"");
        assert_eq!(
            serde_json::to_string(&ShrinkMode::Aggressive).unwrap(),
            "\"aggressive\"",
        );
    }

    #[test]
    fn strategy_kebab_case_round_trips() {
        for strategy in [Strategy::SpecFile, Strategy::Wrap] {
            assert_eq!(round_trip(&strategy), strategy);
        }
    }

    #[test]
    fn strategy_serializes_with_kebab_case_separator() {
        assert_eq!(
            serde_json::to_string(&Strategy::SpecFile).unwrap(),
            "\"spec-file\"",
        );
        assert_eq!(serde_json::to_string(&Strategy::Wrap).unwrap(), "\"wrap\"");
    }

    #[test]
    fn default_record_round_trips() {
        let record = ShrinkRecord::default();
        assert_eq!(record.mode, ShrinkMode::Auto);
        assert_eq!(record.resolved, ShrinkMode::Auto);
        assert!(record.applied.is_empty());
        assert_eq!(record.strategy, None);

        assert_eq!(round_trip(&record), record);
    }

    #[test]
    fn populated_record_round_trips() {
        let record = ShrinkRecord {
            mode: ShrinkMode::Auto,
            resolved: ShrinkMode::Safe,
            applied: vec!["printf-thin".into(), "scanf-thin".into()],
            strategy: Some(Strategy::SpecFile),
        };
        assert_eq!(round_trip(&record), record);
    }

    #[test]
    fn strategy_none_is_omitted_from_json() {
        let record = ShrinkRecord {
            mode: ShrinkMode::Off,
            resolved: ShrinkMode::Off,
            applied: vec![],
            strategy: None,
        };
        let json = serde_json::to_string(&record).expect("serialize");
        assert!(
            !json.contains("strategy"),
            "expected `strategy` to be omitted when None; got: {json}",
        );
    }

    #[test]
    fn missing_applied_defaults_to_empty() {
        // Forward-compatibility: an older writer that didn't emit `applied`
        // must still deserialize.
        let json = r#"{"mode":"safe","resolved":"safe"}"#;
        let record: ShrinkRecord = serde_json::from_str(json).expect("deserialize");
        assert_eq!(record.mode, ShrinkMode::Safe);
        assert_eq!(record.resolved, ShrinkMode::Safe);
        assert!(record.applied.is_empty());
        assert_eq!(record.strategy, None);
    }
}
