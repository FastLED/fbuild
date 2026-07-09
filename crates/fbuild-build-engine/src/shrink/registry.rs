//! Per-platform shrinker registry (FastLED/fbuild#493, #498).
//!
//! Each platform has an associated list of [`AutoShrinkEntry`] values that
//! describe what `--shrink=auto` will apply to it. The entries are what the
//! green `auto shrinking: …` one-liner enumerates at the top of every build.
//!
//! Phase 1d (this file) lands the type and a per-platform lookup function
//! that returns an empty slice for every platform. Phases 4–6 populate the
//! registry per-platform as each shrinker (printf-thin, scanf-thin, stdio
//! FILE strip, …) lands.

use fbuild_core::Platform;

/// A single shrinker the auto-resolver applies on a given platform.
///
/// The `category` is the short human-readable name (`"printf-thin"`,
/// `"scanf-thin"`, `"stdio-file-strip"`, …) used by the verbose plan. The
/// `symbols` slice is what the green one-liner enumerates — typically the
/// names of the libc entry points being shadowed (`vfprintf`, `vsnprintf`,
/// `printf`, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoShrinkEntry {
    /// Short human-readable name of the shrinker (e.g. `"printf-thin"`).
    pub category: &'static str,
    /// Libc/runtime symbols this shrinker shadows or wraps, printed in the
    /// green `auto shrinking:` line.
    pub symbols: &'static [&'static str],
}

/// Per-platform shrinker registry lookup.
///
/// Returns the list of [`AutoShrinkEntry`] values that the auto-resolver
/// would apply on the given platform under `--shrink=auto` (when the
/// per-platform decision resolves to `safe`).
///
/// Phase 1d: every platform returns `&[]`, so the green one-liner stays
/// silent on every build. Phases 4–6 populate this as platforms are
/// validated end-to-end.
#[must_use]
pub fn registry_for(_platform: Platform) -> &'static [AutoShrinkEntry] {
    &[]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_platform_returns_empty_in_phase_1d() {
        for platform in [
            Platform::Apollo3,
            Platform::AtmelAvr,
            Platform::AtmelMegaAvr,
            Platform::AtmelSam,
            Platform::Ch32v,
            Platform::Espressif32,
            Platform::Espressif8266,
            Platform::NordicNrf52,
            Platform::NxpLpc,
            Platform::RaspberryPi,
            Platform::RenesasRa,
            Platform::SiliconLabs,
            Platform::Ststm32,
            Platform::Teensy,
            Platform::Wasm,
        ] {
            assert!(
                registry_for(platform).is_empty(),
                "Phase 1d registry must be empty for every platform; got {} entries for {platform:?}",
                registry_for(platform).len(),
            );
        }
    }

    #[test]
    fn auto_shrink_entry_struct_round_trips() {
        // Smoke test the struct shape that downstream phases will populate.
        const ENTRY: AutoShrinkEntry = AutoShrinkEntry {
            category: "printf-thin",
            symbols: &["vfprintf", "vsnprintf"],
        };
        assert_eq!(ENTRY.category, "printf-thin");
        assert_eq!(ENTRY.symbols.len(), 2);
        assert_eq!(ENTRY.symbols[0], "vfprintf");
    }
}
