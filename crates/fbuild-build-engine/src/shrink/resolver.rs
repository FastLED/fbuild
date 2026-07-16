//! Per-context auto-resolver (FastLED/fbuild#493, #504).
//!
//! `resolve_explicit` (in [`super`]) translates the global / per-subcommand
//! `--shrink` flags into a single [`ShrinkMode`] without per-platform
//! knowledge. This module takes that mode plus a [`ShrinkContext`]
//! (platform + libc + framework) and decides what the build should actually
//! apply.
//!
//! Phase 1c (this file) lands the resolver shell:
//!
//! - Explicit modes ([`ShrinkMode::Off`], [`ShrinkMode::Safe`],
//!   [`ShrinkMode::Aggressive`], [`ShrinkMode::Printf`]) pass through
//!   unchanged — the user told us what they want.
//! - [`ShrinkMode::Auto`] always resolves to [`ShrinkMode::Off`] in Phase 1c,
//!   regardless of context. The real per-platform / per-framework /
//!   per-libc matrix lands in Phase 4+ as each platform is validated
//!   end-to-end and its registry entry is populated.

use fbuild_core::Platform;

use super::ShrinkMode;
use super::probe::Libc;

/// Inputs the auto-resolver consults to turn [`ShrinkMode::Auto`] into a
/// concrete decision.
///
/// Phase 1c does not consult any of the fields — every `Auto` resolves to
/// `Off`. The struct is in place so subsequent phases can populate the
/// real decision matrix without touching every call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShrinkContext<'a> {
    /// Target platform dispatch (AVR / ESP32 / NRF52 / ...).
    pub platform: Platform,
    /// Result of the libc probe (newlib / picolibc / unknown).
    pub libc: Libc,
    /// Framework string from `platformio.ini` (e.g. `"arduino"`,
    /// `"espidf"`, `"mbed"`), or `None` when not detected.
    pub framework: Option<&'a str>,
}

impl<'a> ShrinkContext<'a> {
    /// Convenience constructor used at call sites that don't yet have a
    /// framework probe.
    #[must_use]
    pub fn new(platform: Platform, libc: Libc) -> Self {
        Self {
            platform,
            libc,
            framework: None,
        }
    }

    /// Attach a framework string to an existing context.
    #[must_use]
    pub fn with_framework(mut self, framework: &'a str) -> Self {
        self.framework = Some(framework);
        self
    }
}

/// Resolve an explicit [`ShrinkMode`] against a [`ShrinkContext`].
///
/// Explicit modes pass through unchanged. [`ShrinkMode::Auto`] always
/// resolves to [`ShrinkMode::Off`] in Phase 1c; subsequent phases populate
/// the real per-platform matrix.
#[must_use]
pub fn resolve_for_context(mode: ShrinkMode, _ctx: &ShrinkContext<'_>) -> ShrinkMode {
    match mode {
        ShrinkMode::Auto => ShrinkMode::Off,
        explicit => explicit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_for(platform: Platform, libc: Libc) -> ShrinkContext<'static> {
        ShrinkContext::new(platform, libc)
    }

    const ALL_PLATFORMS: [Platform; 15] = [
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
    ];

    const ALL_LIBC: [Libc; 3] = [Libc::Newlib, Libc::Picolibc, Libc::Unknown];

    #[test]
    fn off_passes_through_unchanged() {
        let ctx = ctx_for(Platform::Espressif32, Libc::Newlib);
        assert_eq!(resolve_for_context(ShrinkMode::Off, &ctx), ShrinkMode::Off);
    }

    #[test]
    fn safe_passes_through_unchanged() {
        let ctx = ctx_for(Platform::Espressif32, Libc::Newlib);
        assert_eq!(
            resolve_for_context(ShrinkMode::Safe, &ctx),
            ShrinkMode::Safe,
        );
    }

    #[test]
    fn aggressive_passes_through_unchanged() {
        let ctx = ctx_for(Platform::Espressif32, Libc::Newlib);
        assert_eq!(
            resolve_for_context(ShrinkMode::Aggressive, &ctx),
            ShrinkMode::Aggressive,
        );
    }

    #[test]
    fn printf_passes_through_unchanged() {
        let ctx = ctx_for(Platform::Espressif32, Libc::Newlib);
        assert_eq!(
            resolve_for_context(ShrinkMode::Printf, &ctx),
            ShrinkMode::Printf,
        );
    }

    #[test]
    fn auto_resolves_to_off_for_every_platform_libc_combo() {
        for platform in ALL_PLATFORMS {
            for libc in ALL_LIBC {
                let ctx = ctx_for(platform, libc);
                assert_eq!(
                    resolve_for_context(ShrinkMode::Auto, &ctx),
                    ShrinkMode::Off,
                    "Phase 1c: Auto must resolve to Off for ({platform:?}, {libc:?})",
                );
            }
        }
    }

    #[test]
    fn auto_resolves_to_off_with_framework_set() {
        let ctx = ShrinkContext::new(Platform::Espressif32, Libc::Newlib).with_framework("arduino");
        assert_eq!(resolve_for_context(ShrinkMode::Auto, &ctx), ShrinkMode::Off,);
    }

    #[test]
    fn explicit_modes_ignore_unfavorable_context() {
        // Even when libc is picolibc (which would be a net-negative target
        // for the printf shadow), an explicit `--shrink=safe` from the user
        // is honored — the resolver only acts on `Auto`.
        let ctx = ctx_for(Platform::Espressif32, Libc::Picolibc);
        assert_eq!(
            resolve_for_context(ShrinkMode::Safe, &ctx),
            ShrinkMode::Safe,
        );
    }

    #[test]
    fn context_constructor_sets_framework_to_none() {
        let ctx = ShrinkContext::new(Platform::AtmelAvr, Libc::Unknown);
        assert_eq!(ctx.platform, Platform::AtmelAvr);
        assert_eq!(ctx.libc, Libc::Unknown);
        assert_eq!(ctx.framework, None);
    }

    #[test]
    fn with_framework_builder_attaches_string() {
        let ctx = ShrinkContext::new(Platform::Espressif32, Libc::Newlib).with_framework("espidf");
        assert_eq!(ctx.framework, Some("espidf"));
    }
}
