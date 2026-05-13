//! Orchestrator-side helper to compute the eh_frame [`EhFramePolicy`] for a
//! single build. Keeps env-var reads and `BuildContext` shape detection out
//! of the pure [`crate::eh_frame_policy::decide`] function, so the pure
//! decision stays trivially testable.
//!
//! See FastLED/fbuild#243 (ESP32) and FastLED/fbuild#244 (other GCC platforms).

use crate::eh_frame_policy::{decide, EhFrameInputs, EhFramePolicy};
use crate::pipeline::BuildContext;
use fbuild_config::sdkconfig::SdkConfigSummary;
use fbuild_core::BuildProfile;

/// Read `FBUILD_KEEP_EH_FRAME` / `FBUILD_STRIP_EH_FRAME` from the process
/// environment, derive the debug-build signal from `BuildContext`, and feed
/// everything into the pure [`decide`] function.
///
/// `sdkconfig` is `Some` only on ESP32 (where sdkconfig keys influence the
/// policy); other platforms pass `None`.
pub(crate) fn compute_eh_frame_policy(
    ctx: &BuildContext,
    profile: BuildProfile,
    sdkconfig: Option<&SdkConfigSummary>,
) -> EhFramePolicy {
    let env_keep = std::env::var("FBUILD_KEEP_EH_FRAME")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let env_strip = std::env::var("FBUILD_STRIP_EH_FRAME")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    // Debug detection: prefer the explicit -Og / -O0 user flag signal because
    // the canonical PlatformIO `build_type = debug` overlay (which adds -Og)
    // has already been applied to `ctx.user_flags` by `BuildContext::new`.
    // -DNDEBUG is the conventional release-mode marker; its absence on a
    // user-flags slate with neither -Og nor -O0 is inconclusive, so we only
    // flag debug on the positive signal.
    let is_debug_build = ctx.user_flags.iter().any(|f| f == "-Og" || f == "-O0");

    let inputs = EhFrameInputs {
        build_flags: &ctx.user_flags,
        build_unflags: &ctx.build_unflags,
        sdkconfig,
        profile,
        is_debug_build,
        env_keep,
        env_strip,
    };
    decide(&inputs)
}
