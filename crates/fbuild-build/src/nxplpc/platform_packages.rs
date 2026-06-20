//! Parser for PlatformIO `platform_packages` framework overrides
//! (FastLED/fbuild#663).
//!
//! `platform_packages` is a multi-line PIO config option whose value lines
//! look like `<name>@<spec>`. For framework packages, the consumer is
//! pinning an upstream source so fbuild can fetch the override commit
//! instead of the const-pinned default. PIO honors several spec forms;
//! the ones relevant here all resolve to a downloadable GitHub archive
//! plus the ref the consumer pinned:
//!
//! - `<owner>/<repo>#<ref>` — GitHub shorthand.
//! - `https://github.com/<owner>/<repo>.git#<ref>` — git URL.
//! - `https://.../<commit>.tar.gz` — direct archive URL (no `#ref`).
//!
//! The parser is scoped to the lpc8xx fix in #663. Future work
//! (FastLED/fbuild#664) will generalize this to every framework package.

use std::collections::HashMap;

/// A resolved framework override: a downloadable archive URL plus the ref
/// (commit / tag) the consumer pinned. The caller turns the ref into a
/// synthetic cache `version` so override installs don't collide with the
/// default pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformPackageOverride {
    pub archive_url: String,
    pub git_ref: String,
}

/// Look up `framework` inside the resolved `[env:*]` config and return
/// the override the consumer pinned, if any.
pub fn lookup_override(
    env_config: &HashMap<String, String>,
    framework: &str,
) -> Option<PlatformPackageOverride> {
    let raw = env_config.get("platform_packages")?;
    parse_platform_packages_entry(raw, framework)
}

/// Scan a `platform_packages` multi-line value for `<framework>@<spec>`
/// and resolve `<spec>` to an archive URL + ref.
pub fn parse_platform_packages_entry(
    raw: &str,
    framework: &str,
) -> Option<PlatformPackageOverride> {
    for line in raw.lines() {
        let line = strip_inline_comment(line.trim());
        if line.is_empty() {
            continue;
        }
        let (name, spec) = match line.split_once('@') {
            Some(pair) => pair,
            None => continue,
        };
        if name.trim() != framework {
            continue;
        }
        if let Some(parsed) = resolve_spec(spec.trim()) {
            return Some(parsed);
        }
    }
    None
}

fn resolve_spec(spec: &str) -> Option<PlatformPackageOverride> {
    if let Some((source, git_ref)) = spec.rsplit_once('#') {
        let source = source.trim();
        let git_ref = git_ref.trim();
        if git_ref.is_empty() {
            return None;
        }

        if let Some(rest) = source.strip_prefix("https://github.com/") {
            let repo = rest.strip_suffix(".git").unwrap_or(rest);
            let repo = repo.trim_end_matches('/');
            if is_owner_repo(repo) {
                return Some(PlatformPackageOverride {
                    archive_url: github_archive_url(repo, git_ref),
                    git_ref: git_ref.to_string(),
                });
            }
        }

        if is_owner_repo(source) {
            return Some(PlatformPackageOverride {
                archive_url: github_archive_url(source, git_ref),
                git_ref: git_ref.to_string(),
            });
        }
    }

    if is_archive_url(spec) {
        let git_ref = spec
            .rsplit('/')
            .next()
            .unwrap_or(spec)
            .trim_end_matches(".tar.gz")
            .trim_end_matches(".tar.bz2")
            .trim_end_matches(".zip")
            .to_string();
        return Some(PlatformPackageOverride {
            archive_url: spec.to_string(),
            git_ref,
        });
    }

    None
}

fn is_owner_repo(s: &str) -> bool {
    let parts: Vec<&str> = s.split('/').collect();
    parts.len() == 2
        && !parts[0].is_empty()
        && !parts[1].is_empty()
        && !s.contains("://")
        && !s.contains(' ')
}

fn is_archive_url(s: &str) -> bool {
    (s.starts_with("http://") || s.starts_with("https://"))
        && (s.ends_with(".tar.gz") || s.ends_with(".tar.bz2") || s.ends_with(".zip"))
}

fn github_archive_url(owner_repo: &str, git_ref: &str) -> String {
    format!(
        "https://github.com/{}/archive/{}.tar.gz",
        owner_repo, git_ref
    )
}

fn strip_inline_comment(s: &str) -> &str {
    for marker in [" ;", " #"] {
        if let Some(idx) = s.find(marker) {
            return s[..idx].trim();
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(value: &str) -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("platform_packages".to_string(), value.to_string());
        m
    }

    #[test]
    fn shorthand_owner_repo_resolves_to_archive_url() {
        let got = parse_platform_packages_entry(
            "framework-arduino-lpc8xx@zackees/ArduinoCore-LPC8xx#195a2ed",
            "framework-arduino-lpc8xx",
        )
        .unwrap();
        assert_eq!(
            got,
            PlatformPackageOverride {
                archive_url: "https://github.com/zackees/ArduinoCore-LPC8xx/archive/195a2ed.tar.gz"
                    .to_string(),
                git_ref: "195a2ed".to_string(),
            }
        );
    }

    #[test]
    fn git_url_with_ref_resolves_to_archive_url() {
        let got = parse_platform_packages_entry(
            "framework-arduino-lpc8xx@https://github.com/zackees/ArduinoCore-LPC8xx.git#deadbee",
            "framework-arduino-lpc8xx",
        )
        .unwrap();
        assert_eq!(
            got.archive_url,
            "https://github.com/zackees/ArduinoCore-LPC8xx/archive/deadbee.tar.gz"
        );
        assert_eq!(got.git_ref, "deadbee");
    }

    #[test]
    fn github_https_url_without_dot_git_still_resolves() {
        let got = parse_platform_packages_entry(
            "framework-arduino-lpc8xx@https://github.com/zackees/ArduinoCore-LPC8xx#abc1234",
            "framework-arduino-lpc8xx",
        )
        .unwrap();
        assert_eq!(
            got.archive_url,
            "https://github.com/zackees/ArduinoCore-LPC8xx/archive/abc1234.tar.gz"
        );
        assert_eq!(got.git_ref, "abc1234");
    }

    #[test]
    fn direct_archive_url_passes_through() {
        let got = parse_platform_packages_entry(
            "framework-arduino-lpc8xx@https://github.com/zackees/ArduinoCore-LPC8xx/archive/195a2ed.tar.gz",
            "framework-arduino-lpc8xx",
        )
        .unwrap();
        assert_eq!(
            got.archive_url,
            "https://github.com/zackees/ArduinoCore-LPC8xx/archive/195a2ed.tar.gz"
        );
        assert_eq!(got.git_ref, "195a2ed");
    }

    #[test]
    fn multiline_value_finds_the_matching_framework() {
        let raw = "\n    other-framework@foo/bar#1\n    framework-arduino-lpc8xx@zackees/ArduinoCore-LPC8xx#deadbee\n";
        let got = parse_platform_packages_entry(raw, "framework-arduino-lpc8xx").unwrap();
        assert_eq!(got.git_ref, "deadbee");
    }

    #[test]
    fn no_match_when_framework_name_differs() {
        let got = parse_platform_packages_entry(
            "framework-other@zackees/ArduinoCore-LPC8xx#deadbee",
            "framework-arduino-lpc8xx",
        );
        assert!(got.is_none());
    }

    #[test]
    fn returns_none_on_empty_value() {
        let got = parse_platform_packages_entry("", "framework-arduino-lpc8xx");
        assert!(got.is_none());
    }

    #[test]
    fn lookup_override_reads_from_env_config() {
        let cfg = env("framework-arduino-lpc8xx@zackees/ArduinoCore-LPC8xx#deadbee");
        let got = lookup_override(&cfg, "framework-arduino-lpc8xx").unwrap();
        assert_eq!(got.git_ref, "deadbee");
    }

    #[test]
    fn lookup_override_returns_none_when_key_missing() {
        let cfg: HashMap<String, String> = HashMap::new();
        assert!(lookup_override(&cfg, "framework-arduino-lpc8xx").is_none());
    }

    #[test]
    fn ignores_comments_and_blank_lines() {
        let raw = "\n# comment\n    ; inline-style\n    framework-arduino-lpc8xx@zackees/ArduinoCore-LPC8xx#deadbee\n";
        let got = parse_platform_packages_entry(raw, "framework-arduino-lpc8xx").unwrap();
        assert_eq!(got.git_ref, "deadbee");
    }

    #[test]
    fn empty_ref_rejected() {
        let got = parse_platform_packages_entry(
            "framework-arduino-lpc8xx@zackees/ArduinoCore-LPC8xx#",
            "framework-arduino-lpc8xx",
        );
        assert!(got.is_none());
    }
}
