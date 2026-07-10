//! The `platformio.ini [env:<id>]` routing triplet.
//!
//! FastLED/fbuild#574. A PlatformIO env block names the three coordinates that
//! determine where every artifact fbuild fetches or generates lives:
//!
//! ```ini
//! [env:lpc845brk]
//! platform  = nxplpc
//! board     = lpc845brk
//! framework = arduino
//! ```
//!
//! `EnvNamespace` captures `(env_id, platform, board, framework)` as one typed
//! value so package fetchers, compile invocations, cache keys, and output dirs
//! can be routed off a single namespace instead of ad-hoc string plumbing.

use crate::Platform;

/// Typed `[env:*]` routing key: the identity of one build environment.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EnvNamespace {
    /// The `[env:<id>]` name, e.g. `lpc845brk`.
    pub env_id: String,
    /// Resolved platform (from the board's platform, e.g. `NxpLpc`).
    pub platform: Platform,
    /// Board id, e.g. `lpc845brk`.
    pub board: String,
    /// Framework string, e.g. `arduino` (empty when the env declares none).
    pub framework: String,
}

impl EnvNamespace {
    pub fn new(
        env_id: impl Into<String>,
        platform: Platform,
        board: impl Into<String>,
        framework: impl Into<String>,
    ) -> Self {
        Self {
            env_id: env_id.into(),
            platform,
            board: board.into(),
            framework: framework.into(),
        }
    }

    /// Filesystem-safe slug identifying this env's per-env artifacts
    /// (`build/<slug>/…`, env-scoped lib cache). Two envs sharing platform but
    /// differing in board/env get distinct slugs; the same env is stable.
    pub fn slug(&self) -> String {
        sanitize(&format!("{}-{}", self.env_id, self.board))
    }

    /// Framework cache segment, e.g. `framework-arduino-nxplpc`. Empty
    /// framework yields `framework-none-<platform>`.
    pub fn framework_segment(&self) -> String {
        let fw = if self.framework.is_empty() {
            "none"
        } else {
            &self.framework
        };
        sanitize(&format!(
            "framework-{}-{}",
            fw,
            format!("{:?}", self.platform).to_ascii_lowercase()
        ))
    }
}

/// Replace anything outside `[A-Za-z0-9._-]` with `_` so the value is safe as a
/// single path segment on every OS.
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_is_stable_and_distinct_per_env() {
        let a = EnvNamespace::new("lpc845brk", Platform::NxpLpc, "lpc845brk", "arduino");
        assert_eq!(a.slug(), "lpc845brk-lpc845brk");
        assert_eq!(a.slug(), a.clone().slug(), "slug must be stable");

        // Same platform, different board+env → different slug.
        let b = EnvNamespace::new(
            "esp32s3-xiao",
            Platform::Espressif32,
            "seeed_xiao_esp32s3",
            "arduino",
        );
        let c = EnvNamespace::new(
            "esp32s3-devkit",
            Platform::Espressif32,
            "esp32-s3-devkitc-1",
            "arduino",
        );
        assert_ne!(b.slug(), c.slug());
    }

    #[test]
    fn framework_segment_covers_none_and_named() {
        let named = EnvNamespace::new("e", Platform::NxpLpc, "b", "arduino");
        assert_eq!(named.framework_segment(), "framework-arduino-nxplpc");
        let none = EnvNamespace::new("e", Platform::NxpLpc, "b", "");
        assert!(none.framework_segment().starts_with("framework-none-"));
    }

    #[test]
    fn slug_sanitizes_unsafe_chars() {
        let ns = EnvNamespace::new("weird/env:name", Platform::AtmelAvr, "b oard", "arduino");
        assert!(!ns.slug().contains('/'));
        assert!(!ns.slug().contains(':'));
        assert!(!ns.slug().contains(' '));
    }
}
