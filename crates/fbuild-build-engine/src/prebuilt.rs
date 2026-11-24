//! What one build request may do with fbuild's prebuilt framework artifacts.
//!
//! `docs/design/artifact-serving-decisions.md` decision 8b gives exact-fingerprint
//! framework **core objects** their own switch, separate from the per-chip
//! library slices of decision 1:
//!
//! - `--no-prebuilt-libs` / `FBUILD_NO_PREBUILT_LIBS` is the **only** public
//!   switch. "Compile framework libraries from source" is the promise the help
//!   text already makes, so everything prebuilt — slices *and* core objects —
//!   folds under it.
//! - While core objects are research-only they are gated by a second, hidden
//!   opt-in: `--prebuilt-core` / `FBUILD_PREBUILT_CORE`.
//! - The opt-out wins over the opt-in, whatever spelling either was given in.
//! - `--prebuilt`, `--no-prebuilt` and `FBUILD_PREBUILT` must never parse.
//!
//! The CLI resolves all of that at argv/env time ([`PrebuiltPolicy::resolve`],
//! [`PrebuiltPolicy::resolve_with_env`]) and forwards the verdict explicitly,
//! because the daemon does not inherit the caller's environment (same rule the
//! shrink mode follows, FastLED/fbuild#1459).
//!
//! Nothing in this module fetches, verifies or extracts an artifact: it is the
//! decision, in one place, so the build path and the CLI notice cannot disagree
//! about what the flags meant.

/// What this build may use from fbuild's prebuilt artifact store.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PrebuiltPolicy {
    /// `--no-prebuilt-libs` / `FBUILD_NO_PREBUILT_LIBS`: nothing prebuilt.
    Disabled,
    /// Default: prebuilt per-chip framework libraries may be used; core objects
    /// stay a research opt-in.
    #[default]
    Libs,
    /// Hidden research opt-in `--prebuilt-core` / `FBUILD_PREBUILT_CORE`.
    Core,
}

impl PrebuiltPolicy {
    /// Resolve an explicit flag pair. The opt-out wins, per decision 8b.
    #[must_use]
    pub fn resolve(no_prebuilt_libs: bool, prebuilt_core: bool) -> Self {
        if no_prebuilt_libs {
            Self::Disabled
        } else if prebuilt_core {
            Self::Core
        } else {
            Self::Libs
        }
    }

    /// Resolve the flag pair against the environment, which is where the
    /// `FBUILD_*` twins live. `env` returns the raw value of one variable so
    /// the truth table is exercised without touching the process environment.
    ///
    /// Precedence is decision 1's: flag > env > default, except that the
    /// opt-out (`--no-prebuilt-libs` *or* `FBUILD_NO_PREBUILT_LIBS=1`) beats the
    /// research opt-in from either source.
    #[must_use]
    pub fn resolve_with_env(
        no_prebuilt_libs: bool,
        prebuilt_core: bool,
        env: impl Fn(&str) -> Option<String>,
    ) -> Self {
        let truthy = |name: &str| {
            env(name)
                .and_then(|value| fbuild_core::env_flag::parse(&value))
                .unwrap_or(false)
        };
        Self::resolve(
            no_prebuilt_libs || truthy("FBUILD_NO_PREBUILT_LIBS"),
            prebuilt_core || truthy("FBUILD_PREBUILT_CORE"),
        )
    }

    /// Stable spelling for the build request / `build_info` trail.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Libs => "libs",
            Self::Core => "core",
        }
    }

    /// Parse [`Self::as_str`]. Unknown text is `None` rather than a default, so
    /// a receiver never silently upgrades a request it did not understand.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "disabled" => Some(Self::Disabled),
            "libs" => Some(Self::Libs),
            "core" => Some(Self::Core),
            _ => None,
        }
    }

    /// Whether per-chip prebuilt library slices may be used at all.
    #[must_use]
    pub fn fetches_prebuilt_libs(self) -> bool {
        !matches!(self, Self::Disabled)
    }

    /// Whether exact-fingerprint prebuilt **core objects** may be served.
    #[must_use]
    pub fn serves_core_objects(self) -> bool {
        matches!(self, Self::Core)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_string())
        }
    }

    #[test]
    fn default_serves_libraries_but_not_core_objects() {
        assert_eq!(PrebuiltPolicy::default(), PrebuiltPolicy::Libs);
        assert!(PrebuiltPolicy::default().fetches_prebuilt_libs());
        assert!(!PrebuiltPolicy::default().serves_core_objects());
    }

    #[test]
    fn opt_in_serves_core_objects_and_keeps_libraries() {
        let policy = PrebuiltPolicy::resolve(false, true);
        assert_eq!(policy, PrebuiltPolicy::Core);
        assert!(policy.fetches_prebuilt_libs());
        assert!(policy.serves_core_objects());
    }

    #[test]
    fn opt_out_wins_over_the_opt_in() {
        assert_eq!(
            PrebuiltPolicy::resolve(true, true),
            PrebuiltPolicy::Disabled
        );
        assert_eq!(
            PrebuiltPolicy::resolve(true, false),
            PrebuiltPolicy::Disabled
        );
        let disabled_with_core_env =
            PrebuiltPolicy::resolve_with_env(true, false, env_of(&[("FBUILD_PREBUILT_CORE", "1")]));
        assert_eq!(disabled_with_core_env, PrebuiltPolicy::Disabled);
        let both_env = PrebuiltPolicy::resolve_with_env(
            false,
            false,
            env_of(&[
                ("FBUILD_NO_PREBUILT_LIBS", "1"),
                ("FBUILD_PREBUILT_CORE", "1"),
            ]),
        );
        assert_eq!(both_env, PrebuiltPolicy::Disabled);
    }

    #[test]
    fn flags_beat_the_environment() {
        let opt_out_env = env_of(&[("FBUILD_NO_PREBUILT_LIBS", "1")]);
        assert_eq!(
            PrebuiltPolicy::resolve_with_env(false, true, opt_out_env),
            PrebuiltPolicy::Disabled
        );
        let core_env = env_of(&[("FBUILD_PREBUILT_CORE", "1")]);
        assert_eq!(
            PrebuiltPolicy::resolve_with_env(false, false, core_env),
            PrebuiltPolicy::Core
        );
        assert_eq!(
            PrebuiltPolicy::resolve_with_env(false, false, env_of(&[])),
            PrebuiltPolicy::Libs
        );
    }

    #[test]
    fn environment_follows_the_canonical_truth_table() {
        for (value, expected) in [
            ("1", PrebuiltPolicy::Disabled),
            ("true", PrebuiltPolicy::Disabled),
            (" YES ", PrebuiltPolicy::Disabled),
            ("on", PrebuiltPolicy::Disabled),
            ("0", PrebuiltPolicy::Libs),
            ("false", PrebuiltPolicy::Libs),
            ("no", PrebuiltPolicy::Libs),
            ("off", PrebuiltPolicy::Libs),
            ("", PrebuiltPolicy::Libs),
            ("ture", PrebuiltPolicy::Libs),
        ] {
            let resolved = PrebuiltPolicy::resolve_with_env(false, false, |name| {
                (name == "FBUILD_NO_PREBUILT_LIBS").then(|| value.to_string())
            });
            assert_eq!(resolved, expected, "FBUILD_NO_PREBUILT_LIBS={value:?}");
        }
    }

    #[test]
    fn wire_spelling_round_trips_and_rejects_anything_else() {
        for policy in [
            PrebuiltPolicy::Disabled,
            PrebuiltPolicy::Libs,
            PrebuiltPolicy::Core,
        ] {
            assert_eq!(PrebuiltPolicy::parse(policy.as_str()), Some(policy));
        }
        for text in ["", "off", "on", "prebuilt", "Disabled", "core "] {
            assert_eq!(PrebuiltPolicy::parse(text), None, "{text:?}");
        }
    }
}
