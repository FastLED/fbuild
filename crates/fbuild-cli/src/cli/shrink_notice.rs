//! Honest reporting for `--shrink` (FastLED/fbuild#1459).
//!
//! `--shrink[=MODE]` has been accepted by the CLI since FastLED/fbuild#496. The
//! only reducer wired to the link stage is ESP32 `printf` (newlib-nano); every
//! other platform still resolves through an empty
//! [`fbuild_build::shrink::registry::registry_for`] slice.
//! A build therefore applies nothing — and says nothing, which is the part that
//! misleads: a user who asks for `--shrink=safe` cannot tell the request was
//! dropped, so the flag looks like it works.
//!
//! ESP32 `--shrink=printf` (newlib-nano, FastLED/fbuild#1459) is reported by
//! the ESP32 orchestrator itself (`shrink_link::resolve_shrink`), because
//! whether it really links is an MCU-architecture question (Xtensa shrinks,
//! RISC-V grows the image) that this CLI cannot answer from `platformio.ini`.
//! Here it only counts as supported, so the "no shrinker" line is suppressed.
//!
//! This module converts that silence into one line per build, and only for an
//! *explicit* request this build cannot honour:
//!
//! - [`ShrinkMode::Auto`] — the default, and what a bare `--shrink` selects —
//!   and [`ShrinkMode::Off`] stay silent. "No shrinking" is the documented
//!   outcome for both today (see [`fbuild_build::shrink::resolver`]).
//! - An explicit `safe` / `aggressive` / `printf` with no registered shrinker
//!   prints `shrink: --shrink=<mode> has no effect yet: …` and names the
//!   platform, so the report points at the missing reducer instead of leaving
//!   the user to discover it from a symbol dump.
//!
//! The notice disappears by itself once a platform's registry gains entries;
//! emitting the applied-shrinker line is then the link stage's job
//! (`print_auto_shrinking_line` in `fbuild-build-engine`).
//!
//! Nothing here decides *what* to shrink — the text is a report of the
//! resolver's verdict, kept in one place so the terminal output and (future)
//! `build_info` telemetry cannot drift apart.

use fbuild_build::shrink::ShrinkMode;
use fbuild_build::shrink::registry::{AutoShrinkEntry, registry_for};
use fbuild_core::Platform;
use fbuild_core::path::NormalizedPath;

/// PlatformIO platform id for `platform`, e.g. `espressif32`.
///
/// Deliberately spelled the way `platformio.ini` spells it so the message can
/// be matched against the project's own `platform = …` line.
fn platform_id(platform: Platform) -> &'static str {
    match platform {
        Platform::Apollo3 => "apollo3",
        Platform::AtmelAvr => "atmelavr",
        Platform::AtmelMegaAvr => "atmelmegaavr",
        Platform::AtmelSam => "atmelsam",
        Platform::Ch32v => "ch32v",
        Platform::Espressif32 => "espressif32",
        Platform::Espressif8266 => "espressif8266",
        Platform::NordicNrf52 => "nordicnrf52",
        Platform::NxpLpc => "nxplpc",
        Platform::RaspberryPi => "raspberrypi",
        Platform::RenesasRa => "renesasra",
        Platform::SiliconLabs => "silabs",
        Platform::Ststm32 => "ststm32",
        Platform::Teensy => "teensy",
        Platform::Wasm => "wasm",
    }
}

/// Kebab-case spelling `clap` accepts for `mode`.
fn mode_name(mode: ShrinkMode) -> &'static str {
    match mode {
        ShrinkMode::Auto => "auto",
        ShrinkMode::Off => "off",
        ShrinkMode::Safe => "safe",
        ShrinkMode::Aggressive => "aggressive",
        ShrinkMode::Printf => "printf",
    }
}

/// The one-line notice for an explicit shrink request this build cannot
/// honour, or `None` when there is nothing useful to say.
///
/// `available` is the platform's registry slice
/// ([`registry_for`] in production, an injected slice in tests). A non-empty
/// slice means a shrinker *is* registered for the platform, so the request is
/// not unhonoured here and this function stays quiet — the link stage owns the
/// applied line.
#[must_use]
pub fn unsupported_notice(
    mode: ShrinkMode,
    platform: Option<Platform>,
    available: &[AutoShrinkEntry],
) -> Option<String> {
    if matches!(mode, ShrinkMode::Auto | ShrinkMode::Off) || !available.is_empty() {
        return None;
    }
    if is_supported(mode, platform) {
        return None;
    }
    let target = platform.map_or("this platform", platform_id);
    Some(format!(
        "shrink: --shrink={} has no effect yet: no shrinker is registered for {target}; \
         building unshrunk (FastLED/fbuild#1459)",
        mode_name(mode)
    ))
}

/// Whether this resolver mode actually reaches the link stage on `platform`.
///
/// Keep in sync with `fbuild_build_esp::esp32::printf_thin::applies`: the
/// ESP32/Xtensa newlib printf reducer is the only implemented shrinker.
#[must_use]
pub fn is_supported(mode: ShrinkMode, platform: Option<Platform>) -> bool {
    mode == ShrinkMode::Printf && platform == Some(Platform::Espressif32)
}

/// Best-effort platform lookup from the project's `platformio.ini`.
///
/// Any failure — unreadable ini, unknown environment, no `platform = ` line —
/// degrades to `None`, which only widens the message text. Reporting must never
/// turn a build into an error.
fn platform_for(project_dir: &str, environment: Option<&str>) -> Option<Platform> {
    let ini = NormalizedPath::new(project_dir).join("platformio.ini");
    let config = fbuild_config::PlatformIOConfig::from_path(ini.as_path()).ok()?;
    let env_name = environment.or_else(|| config.get_default_environment())?;
    let env_config = config.get_env_config(env_name).ok()?;
    env_config
        .get("platform")
        .and_then(|value| Platform::from_platform_str(value))
}

/// Report the resolved shrink decision for a `fbuild build` invocation.
///
/// Emits at most one warning line. Silence (the common case) is intentional:
/// `auto`/`off` are the honest outcomes today, and a line on every build would
/// be noise.
pub fn report_build_shrink(project_dir: &str, environment: Option<&str>, mode: ShrinkMode) {
    let platform = platform_for(project_dir, environment);
    let available = platform.map_or(&[][..], registry_for);
    // The applied-reducer line is NOT printed here: whether the reducer really
    // links depends on the MCU architecture (Xtensa shrinks, RISC-V grows), and
    // only the ESP32 orchestrator can resolve that. It reports the applied
    // line from `shrink_link::resolve_shrink`; this CLI-side notice is limited
    // to "no shrinker exists for this platform".
    if let Some(line) = unsupported_notice(mode, platform, available) {
        // `output::warn` is a `tracing::warn!`, which only appears when a
        // `RUST_LOG` filter raises the level — a warning nobody sees would
        // recreate the silent-flag problem this module exists to fix.
        // `diagnostic` with the `WARNING:` marker is the always-visible path.
        crate::output::diagnostic(format!("WARNING: {line}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fbuild_build::shrink::probe::Libc;
    use fbuild_build::shrink::resolver::{ShrinkContext, resolve_for_context};

    const PRINTF_THIN: AutoShrinkEntry = AutoShrinkEntry {
        category: "printf-thin",
        symbols: &["vfprintf", "vsnprintf"],
    };

    #[test]
    fn auto_is_silent() {
        assert_eq!(
            unsupported_notice(ShrinkMode::Auto, Some(Platform::Espressif32), &[]),
            None
        );
    }

    #[test]
    fn off_is_silent() {
        assert_eq!(
            unsupported_notice(ShrinkMode::Off, Some(Platform::Espressif32), &[]),
            None
        );
    }

    #[test]
    fn esp32_printf_is_not_reported_as_unsupported() {
        assert!(is_supported(
            ShrinkMode::Printf,
            Some(Platform::Espressif32)
        ));
        assert_eq!(
            unsupported_notice(ShrinkMode::Printf, Some(Platform::Espressif32), &[]),
            None
        );
    }

    #[test]
    fn printf_on_other_platforms_stays_unsupported() {
        for platform in [
            Platform::AtmelAvr,
            Platform::Espressif8266,
            Platform::RaspberryPi,
        ] {
            assert!(!is_supported(ShrinkMode::Printf, Some(platform)));
            assert!(unsupported_notice(ShrinkMode::Printf, Some(platform), &[]).is_some());
        }
        assert!(unsupported_notice(ShrinkMode::Printf, None, &[]).is_some());
    }

    #[test]
    fn explicit_mode_with_empty_registry_is_reported_with_platform() {
        let line = unsupported_notice(ShrinkMode::Safe, Some(Platform::Espressif32), &[])
            .expect("an unhonoured explicit request must be reported");
        assert!(line.contains("--shrink=safe"), "{line}");
        assert!(line.contains("no shrinker is registered"), "{line}");
        assert!(line.contains("espressif32"), "{line}");
        assert!(line.contains("FastLED/fbuild#1459"), "{line}");
    }

    #[test]
    fn every_explicit_non_off_mode_is_reported() {
        for mode in [ShrinkMode::Safe, ShrinkMode::Aggressive] {
            let line = unsupported_notice(mode, Some(Platform::Espressif32), &[])
                .unwrap_or_else(|| panic!("{mode:?} must be reported"));
            assert!(
                line.contains(&format!("--shrink={}", mode_name(mode))),
                "{line}"
            );
        }
    }

    #[test]
    fn unknown_platform_still_reports() {
        let line = unsupported_notice(ShrinkMode::Printf, None, &[])
            .expect("must report without a resolved platform");
        assert!(line.contains("this platform"), "{line}");
    }

    #[test]
    fn registered_shrinker_suppresses_the_notice() {
        // Once a platform's registry is populated the link stage reports what it
        // applied; the CLI must not claim the flag was ignored.
        assert_eq!(
            unsupported_notice(
                ShrinkMode::Safe,
                Some(Platform::Espressif32),
                &[PRINTF_THIN]
            ),
            None
        );
    }

    #[test]
    fn esp32_s3_newlib_auto_is_off_today() {
        // The RED half of #1459, pinned as a test: `--shrink=auto` on an
        // ESP32-S3/newlib/arduino context resolves to `Off`, so a stock Blink
        // build keeps `_vfprintf_r`. When the printf reducer lands this test is
        // the one that must change.
        let ctx = ShrinkContext::new(Platform::Espressif32, Libc::Newlib).with_framework("arduino");
        assert_eq!(resolve_for_context(ShrinkMode::Auto, &ctx), ShrinkMode::Off);
        assert_eq!(
            registry_for(Platform::Espressif32),
            &[],
            "no ESP32 shrinker is registered yet"
        );
    }

    #[test]
    fn platform_id_covers_every_slice_of_the_report() {
        assert_eq!(platform_id(Platform::Espressif32), "espressif32");
        assert_eq!(platform_id(Platform::RaspberryPi), "raspberrypi");
        assert_eq!(platform_id(Platform::AtmelAvr), "atmelavr");
    }
}
