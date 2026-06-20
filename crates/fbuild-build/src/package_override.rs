//! Shared `platform_packages` override resolver for every framework orchestrator.
//!
//! PlatformIO honors `platform_packages = framework-x@<URL>#<sha>` on the env
//! section; fbuild's framework packages used to ignore it (audit: #664; first
//! report: #663). The per-orchestrator wiring is now a uniform three-line delta:
//!
//! ```ignore
//! let core = match package_override::resolve_override(env_config, "framework-arduino-lpc8xx") {
//!     Some(ovr) => ArduinoCoreLpc8xx::with_override(&params.project_dir, ovr),
//!     None      => ArduinoCoreLpc8xx::new(&params.project_dir),
//! };
//! ```
//!
//! Centralizing the lookup here means every framework orchestrator gets the
//! same parsing behavior — there is no per-platform place to forget multi-line
//! handling, owner/repo expansion, or empty-value tolerance.

use std::collections::HashMap;

use fbuild_config::PackageOverride;

/// Look up a `platform_packages` override for `package_name` in the resolved
/// env config (`PlatformIOConfig::get_env_config(env)`).
///
/// Returns `None` when the env has no `platform_packages` key, or when no entry
/// in that value matches `package_name`. Multi-line values are scanned in order
/// and the first match wins (PlatformIO semantics).
pub fn resolve_override(
    env_config: &HashMap<String, String>,
    package_name: &str,
) -> Option<PackageOverride> {
    let raw = env_config.get("platform_packages")?;
    fbuild_config::parse_platform_packages_value(raw, package_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn returns_override_when_env_has_matching_entry() {
        let env = env(&[(
            "platform_packages",
            "framework-arduino-lpc8xx@https://github.com/zackees/ArduinoCore-LPC8xx/archive/aaaabbbbccccddddeeeeffff0000111122223333.tar.gz#aaaabbbbccccddddeeeeffff0000111122223333",
        )]);
        let ovr = resolve_override(&env, "framework-arduino-lpc8xx").expect("override resolved");
        assert!(ovr.url.contains("ArduinoCore-LPC8xx"));
        assert_eq!(ovr.version, "0.0.0+gaaaabbb");
    }

    #[test]
    fn returns_none_when_platform_packages_key_absent() {
        let env = env(&[("build_flags", "-DFOO=1")]);
        assert!(resolve_override(&env, "framework-arduino-lpc8xx").is_none());
    }

    #[test]
    fn returns_none_when_no_entry_matches_package_name() {
        let env = env(&[(
            "platform_packages",
            "framework-some-other-thing@https://example.com/archive/abc.tar.gz#abc",
        )]);
        assert!(resolve_override(&env, "framework-arduino-lpc8xx").is_none());
    }

    #[test]
    fn multi_line_value_picks_first_matching_entry() {
        // PlatformIO INI parsing joins continuation lines with `\n`; the
        // helper must scan all of them and return the first match.
        let env = env(&[(
            "platform_packages",
            "framework-arduino-lpc8xx@zackees/ArduinoCore-LPC8xx#deadbeefdeadbeefdeadbeefdeadbeefdeadbeef\nframework-arduino-lpc8xx@zackees/ArduinoCore-LPC8xx#cafef00dcafef00dcafef00dcafef00dcafef00d",
        )]);
        let ovr = resolve_override(&env, "framework-arduino-lpc8xx").unwrap();
        assert_eq!(ovr.version, "0.0.0+gdeadbee");
    }

    #[test]
    fn version_pin_only_returns_none() {
        // `name @ 1.2.3` is a registry version pin, not a URL override.
        let env = env(&[("platform_packages", "framework-arduino-lpc8xx @ 1.2.3")]);
        assert!(resolve_override(&env, "framework-arduino-lpc8xx").is_none());
    }
}
