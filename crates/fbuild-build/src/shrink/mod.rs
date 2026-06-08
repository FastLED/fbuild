//! Flash-size reduction for embedded firmware (FastLED/fbuild#493).
//!
//! This module hosts the `--shrink[=MODE]` flag, the per-platform applier
//! registry, the libc probe, the auto-resolver, and the spec-file /
//! shadow-archive / wrap-fallback link strategies.
//!
//! Phase 1a (FastLED/fbuild#496) lands the [`ShrinkMode`] enum and the
//! [`resolve_explicit`] helper that combines the global `--shrink` flag, the
//! per-subcommand `--shrink` flag, and `--no-shrink` into a single value.
//!
//! Phase 1d (FastLED/fbuild#498) lands the per-platform [`registry`] and the
//! green `auto shrinking:` one-liner [`reporting`]. Every platform's registry
//! is empty in Phase 1d, so the reporter stays silent on every build until
//! subsequent phases populate it.
//!
//! `ShrinkMode` is deliberately clap-free so `fbuild-build` does not pick up a
//! clap dependency. The CLI layer in `fbuild-cli` defines a tiny mirror enum
//! with `#[derive(clap::ValueEnum)]` and converts via `From`.

pub mod probe;
pub mod registry;
pub mod reporting;
pub mod resolver;

/// User-facing shrink level for `fbuild build --shrink[=MODE]`.
///
/// See FastLED/fbuild#493 for the full per-mode contract. Phase 1a treats
/// every variant as a no-op at the link level — only [`Off`](Self::Off) is
/// observably distinct from "no flag", because the auto-resolver returns
/// `Off` for every platform in this phase.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ShrinkMode {
    /// Decide per framework + IDF version + libc probe. Default when the user
    /// passes `--shrink` with no value or omits the flag entirely.
    #[default]
    Auto,
    /// Disable all shrinking; use stock libc, full symbol set. Same as
    /// `--no-shrink`.
    Off,
    /// No-behavior-change wins only: printf-thin wrap, scanf-thin wrap,
    /// stdio FILE strip.
    Safe,
    /// `Safe` plus behavior-altering wins: `esp_err_msg_table` strip, coredump
    /// disable, `-Oz`. Requires sdkconfig rebuild on ESP32.
    Aggressive,
    /// Single-knob debugging: only the printf-family wrap via the
    /// `-Wl,--wrap=` fallback path. Useful for A/B-comparing the printf swap
    /// against `Safe` (which uses the spec-file strategy).
    Printf,
}

/// Combine the CLI inputs into a single resolved [`ShrinkMode`].
///
/// Precedence (highest wins):
/// 1. `--no-shrink` always wins and resolves to [`ShrinkMode::Off`].
/// 2. Subcommand-level `--shrink=<MODE>` overrides global `--shrink`.
/// 3. Global `--shrink=<MODE>` from the top-level `Cli`.
/// 4. Nothing → [`ShrinkMode::Auto`].
///
/// Per-platform resolution of `Auto` → `Safe` / `Off` lands in subsequent
/// phases; this helper only deals with explicit user intent.
pub fn resolve_explicit(
    global: Option<ShrinkMode>,
    subcommand: Option<ShrinkMode>,
    no_shrink: bool,
) -> ShrinkMode {
    if no_shrink {
        return ShrinkMode::Off;
    }
    subcommand.or(global).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_flag_resolves_to_auto() {
        assert_eq!(resolve_explicit(None, None, false), ShrinkMode::Auto);
    }

    #[test]
    fn no_shrink_always_wins() {
        assert_eq!(
            resolve_explicit(Some(ShrinkMode::Aggressive), Some(ShrinkMode::Safe), true),
            ShrinkMode::Off,
        );
    }

    #[test]
    fn subcommand_overrides_global() {
        assert_eq!(
            resolve_explicit(Some(ShrinkMode::Safe), Some(ShrinkMode::Aggressive), false),
            ShrinkMode::Aggressive,
        );
    }

    #[test]
    fn global_used_when_subcommand_absent() {
        assert_eq!(
            resolve_explicit(Some(ShrinkMode::Aggressive), None, false),
            ShrinkMode::Aggressive,
        );
    }

    #[test]
    fn default_is_auto() {
        assert_eq!(ShrinkMode::default(), ShrinkMode::Auto);
    }
}
