//! Honest reporting for the prebuilt-artifact switches (FastLED/fbuild#1882).
//!
//! `docs/design/artifact-serving-decisions.md` decision 8b fixes the flag
//! surface — one public opt-out (`--no-prebuilt-libs`) and one hidden research
//! opt-in (`--prebuilt-core`) — before the core-object serving ladder exists.
//! A flag that is accepted and then does nothing, silently, is exactly the
//! failure mode `shrink_notice` was written to prevent (FastLED/fbuild#1459):
//! the user cannot tell whether the switch worked.
//!
//! So both explicit decisions are reported once per build:
//!
//! - `--prebuilt-core` (or `FBUILD_PREBUILT_CORE=1`) — the fingerprint and the
//!   component stamps exist (see `esp32_framework::prebuilt_core`), but the
//!   fetch ladder that would turn a fingerprint into objects does not, so the
//!   build compiles the framework from source. Said as a warning, because the
//!   requested behaviour is missing.
//! - `--no-prebuilt-libs` (or `FBUILD_NO_PREBUILT_LIBS=1`) — the build
//!   compiles framework libraries from source and the SDK download takes the
//!   whole published bundle instead of a pre-built per-chip slice, so the
//!   verdict now changes the fetch itself and not only this line
//!   (FastLED/fbuild#2196). Said as a plain result line, because that *is*
//!   what happens.
//!
//! The default ([`PrebuiltPolicy::Libs`]) stays silent: it is what every build
//! does, and a line per build would be noise. Both notices disappear as the
//! ladder lands — no caller has to remember to delete them, the policy does.

use fbuild_build::prebuilt::PrebuiltPolicy;

/// The one line to show for an explicit prebuilt decision, or `None` for the
/// default (nothing to say).
#[must_use]
pub fn decision_notice(policy: PrebuiltPolicy) -> Option<String> {
    match policy {
        PrebuiltPolicy::Libs => None,
        PrebuiltPolicy::Disabled => Some(
            "prebuilt: --no-prebuilt-libs — compiling framework libraries from source; \
             the SDK download takes the whole published bundle instead of a \
             pre-built per-chip slice (FastLED/fbuild#1882)"
                .to_string(),
        ),
        PrebuiltPolicy::Core => Some(
            "WARNING: prebuilt: --prebuilt-core is a research opt-in — exact-fingerprint \
             core-object serving is not implemented yet, so the framework compiles from \
             source (FastLED/fbuild#1882)"
                .to_string(),
        ),
    }
}

/// Resolve the two CLI flags against this process's environment.
///
/// The environment is read here — at the CLI boundary — and never deeper: the
/// daemon does not inherit the caller's environment, so the verdict has to be
/// decided before the request exists (decision 1).
#[must_use]
pub fn resolve_from_cli(no_prebuilt_libs: bool, prebuilt_core: bool) -> PrebuiltPolicy {
    PrebuiltPolicy::resolve_with_env(no_prebuilt_libs, prebuilt_core, |name| {
        std::env::var(name).ok()
    })
}

/// Report the resolved prebuilt decision for a `fbuild build` invocation.
pub fn report_build_prebuilt(policy: PrebuiltPolicy) {
    if let Some(line) = decision_notice(policy) {
        crate::output::diagnostic(line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_is_silent() {
        assert_eq!(decision_notice(PrebuiltPolicy::Libs), None);
    }

    #[test]
    fn research_opt_in_says_serving_is_missing() {
        let line = decision_notice(PrebuiltPolicy::Core).expect("core notice");
        assert!(line.contains("--prebuilt-core"), "{line}");
        assert!(line.contains("not implemented yet"), "{line}");
        assert!(line.contains("source"), "{line}");
        assert!(line.starts_with("WARNING:"), "{line}");
        assert!(line.contains("#1882"), "{line}");
    }

    #[test]
    fn opt_out_states_the_source_compile() {
        let line = decision_notice(PrebuiltPolicy::Disabled).expect("disabled notice");
        assert!(line.contains("--no-prebuilt-libs"), "{line}");
        assert!(line.contains("from source"), "{line}");
        assert!(line.contains("whole published bundle"), "{line}");
        assert!(!line.starts_with("WARNING:"), "{line}");
    }
}
