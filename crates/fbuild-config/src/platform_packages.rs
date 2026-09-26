//! Parsing for the PlatformIO `platform_packages` directive.
//!
//! PlatformIO lets a consumer override a framework package's source by pinning
//! a URL + commit on the env's `platform_packages` line:
//!
//! ```ini
//! [env:lpc845brk]
//! platform_packages =
//!     framework-arduino-lpc8xx@https://github.com/owner/repo/archive/<sha>.tar.gz
//!     framework-arduino-lpc8xx@owner/repo#<sha>
//! ```
//!
//! fbuild's framework packages used to ignore this line entirely (every package
//! pinned its URL + commit + sha256 as `const &str`). FastLED/fbuild#664
//! audited the gap across all 16 framework packages and #681 is the
//! implementation follow-up: parsing lives here, the `PackageBase` abstraction
//! consumes the parsed `PackageOverride`, and per-orchestrator wiring is a
//! uniform 3-line delta.
//!
//! The original LPC8xx bisection workflow that motivated this — see
//! FastLED/fbuild#663 and FastLED/FastLED#3325 — is unblocked once the
//! consumer's `platform_packages` line reaches `PackageBase::with_override`.

/// A consumer-supplied override for a `PackageBase`.
///
/// Constructed by [`parse_platform_packages_entry`] (or by tests). Applied
/// via `PackageBase::with_override`, which derives a distinct cache subdir
/// from `url` so the override never collides with the default-pinned cache.
///
/// `checksum` is `None` for overrides parsed from `platform_packages`: the
/// consumer is explicitly overriding for development / bisection and we don't
/// expect them to recompute sha256 per pin. The cache key derivation
/// (URL → stem + sha256-prefix in `fbuild_packages::Cache`) is what guarantees
/// uniqueness — two different commit URLs hash to two different directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageOverride {
    /// Full download URL (typically `https://github.com/<owner>/<repo>/archive/<sha>.tar.gz`).
    pub url: String,
    /// Cache-subdir-discriminating version string. Conventionally `<base>+g<short-sha>`
    /// so the override's cache path doesn't collide with the default pin.
    pub version: String,
    /// Optional SHA-256 of the archive. `None` skips checksum verification —
    /// the consumer is explicitly trusting the pin.
    pub checksum: Option<String>,
}

impl PackageOverride {
    /// Convenience constructor with no checksum (the common case for parsed
    /// `platform_packages` lines).
    pub fn new(url: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            version: version.into(),
            checksum: None,
        }
    }
}

/// Parse a single `platform_packages` line for an entry matching `package_name`.
///
/// PlatformIO syntaxes accepted:
///
///   `name@<URL>#<sha>`              → uses `<URL>` as the download URL and `<sha>` as the version discriminator
///   `name@<URL>` (no `#sha`)        → uses `<URL>` as the download URL; version derives from the URL tail
///   `name@<owner>/<repo>#<sha>`     → expands to the GitHub archive tarball URL for that sha
///   `name @ <version>`              → returns `None` (version pin, not a URL override)
///   anything else                   → returns `None`
///
/// Returns `None` when `package_name` does not match the entry's name.
///
/// Whitespace around the `@` separator and around the line itself is tolerated.
pub fn parse_platform_packages_entry(line: &str, package_name: &str) -> Option<PackageOverride> {
    let line = line.trim().trim_end_matches([',', ';']).trim();
    if line.is_empty() {
        return None;
    }

    // Split on the first `@`. The `name` side may have whitespace around it
    // (e.g. `name @ value`).
    let (name, rest) = line.split_once('@')?;
    let name = name.trim();
    if name != package_name {
        return None;
    }
    let rest = rest.trim();
    if rest.is_empty() {
        return None;
    }

    // Plain `name @ <version>` (no URL, no slash, no `#sha`) is a registry
    // version pin, not an override we can act on.
    let looks_like_url = rest.starts_with("http://") || rest.starts_with("https://");
    let looks_like_owner_repo = rest.contains('/') && rest.contains('#');
    if !looks_like_url && !looks_like_owner_repo {
        return None;
    }

    // Split off `#<sha>` if present.
    let (target, sha) = match rest.split_once('#') {
        Some((t, s)) => (t.trim(), Some(s.trim())),
        None => (rest, None),
    };

    let (url, version) = if looks_like_url {
        let url = target.to_string();
        let version = match sha {
            Some(s) if !s.is_empty() => version_string(s),
            _ => version_from_url_tail(target),
        };
        (url, version)
    } else {
        // owner/repo#sha → GitHub archive URL
        let sha = sha?;
        if sha.is_empty() {
            return None;
        }
        let owner_repo = target;
        if owner_repo.matches('/').count() != 1 {
            return None;
        }
        let url = format!("https://github.com/{}/archive/{}.tar.gz", owner_repo, sha);
        (url, version_string(sha))
    };

    Some(PackageOverride {
        url,
        version,
        checksum: None,
    })
}

/// Scan a multi-line `platform_packages` value and return the first override
/// matching `package_name`, if any.
///
/// `value` is the raw INI value as returned by `PlatformIOConfig::get_env_config`
/// (multi-line PlatformIO values are joined with `\n` by the parser).
pub fn parse_platform_packages_value(value: &str, package_name: &str) -> Option<PackageOverride> {
    value
        .lines()
        .filter_map(|line| parse_platform_packages_entry(line, package_name))
        .next()
}

/// Describe every registry version pin in an env that fbuild will not honor.
///
/// fbuild has no PlatformIO registry resolver, so `platform = espressif32@6.5.0`
/// and a bare `platform_packages = <name>@<version>` both build against the
/// packages fbuild pins itself. That used to happen silently, which left the
/// ini claiming a version that was never built (FastLED/fbuild#1407). Each
/// returned message names the ignored pin and the URL form that does work;
/// callers surface them in the build output.
pub fn ignored_version_pins(env: &std::collections::HashMap<String, String>) -> Vec<String> {
    let mut warnings = Vec::new();
    let platform = env.get("platform").map(|v| v.trim()).unwrap_or_default();
    let is_esp32 = fbuild_core::Platform::from_platform_str(platform)
        == Some(fbuild_core::Platform::Espressif32);
    let hint = if is_esp32 {
        "To pin ESP32, use a URL: `platform = https://github.com/pioarduino/platform-espressif32/releases/download/<tag>/platform-espressif32.zip` or `platform_packages = platform-espressif32@<archive URL>`."
    } else {
        "To pin a package, give its archive URL: `platform_packages = <package>@<URL>` or `<package>@<owner>/<repo>#<sha>`."
    };

    if let Some((name, version)) = registry_version_pin(platform) {
        warnings.push(format!(
            "`platform = {platform}`: version pin `{version}` is ignored; fbuild does not \
             resolve PlatformIO registry versions and builds with its own pinned `{name}` \
             packages. {hint}"
        ));
    }

    if let Some(raw) = env.get("platform_packages") {
        for line in raw.lines() {
            let entry = line.trim().trim_end_matches([',', ';']).trim();
            if let Some((name, version)) = registry_version_pin(entry) {
                warnings.push(format!(
                    "`platform_packages = {entry}`: version pin `{version}` is ignored; \
                     fbuild does not resolve PlatformIO registry versions and uses its own \
                     pinned `{name}`. {hint}"
                ));
            }
        }
    }
    warnings
}

/// Split `name@<version>` when the right-hand side is a registry version, not
/// one of the URL / `owner/repo#sha` forms [`parse_platform_packages_entry`]
/// honors.
fn registry_version_pin(value: &str) -> Option<(&str, &str)> {
    if value.contains("://") {
        return None;
    }
    let (name, version) = value.split_once('@')?;
    let (name, version) = (name.trim(), version.trim());
    if name.is_empty() || version.is_empty() || (version.contains('/') && version.contains('#')) {
        return None;
    }
    Some((name, version))
}

/// Archive extensions fbuild can download and unpack.
const ARCHIVE_EXTENSIONS: [&str; 4] = [".zip", ".tar.gz", ".tar.bz2", ".tar.xz"];

/// Parse an env's `platform = <value>` as a pinned, downloadable platform archive.
///
/// PlatformIO accepts a release archive URL as the `platform` value, and that is
/// how consumers pin a pioarduino release. Only `http(s)` URLs ending in an
/// archive extension qualify, because those are what fbuild can fetch. Registry
/// pins (`espressif32@6.5.0`), bare names and git URLs return `None`.
pub fn parse_platform_archive_url(value: &str) -> Option<PackageOverride> {
    let url = value.trim();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return None;
    }
    if !ARCHIVE_EXTENSIONS.iter().any(|ext| url.ends_with(ext)) {
        return None;
    }
    Some(PackageOverride::new(url, version_from_url_tail(url)))
}

fn version_string(sha: &str) -> String {
    // `0.0.0+g<short-sha>` — keeps the cache-subdir distinct from the default
    // pin (which uses its own `<base>+g<short-sha>` pattern). The `0.0.0+` base
    // is a placeholder; the `+g<short>` build-metadata is what differentiates.
    let short = sha.get(..7).unwrap_or(sha);
    format!("0.0.0+g{}", short)
}

/// The `<tag>` of a GitHub `.../releases/download/<tag>/<asset>` URL, when it
/// is safe to use as a cache path segment.
fn release_tag(url: &str) -> Option<&str> {
    let (_, rest) = url.split_once("/releases/download/")?;
    let tag = rest.split('/').next()?;
    let path_safe = tag
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'));
    // `.` and `..` pass the character check but name the cache directory
    // itself or its parent.
    let names_a_directory = matches!(tag, "" | "." | "..");
    (path_safe && !names_a_directory).then_some(tag)
}

fn version_from_url_tail(url: &str) -> String {
    // A release asset URL names its version in the tag segment.
    if let Some(tag) = release_tag(url) {
        return tag.to_string();
    }
    // For `https://.../archive/<sha>.tar.gz` (no explicit `#sha`), pull the
    // sha out of the URL tail. Falls back to a generic `override` tag if the
    // tail isn't a recognizable sha-shaped token.
    let last = url.rsplit('/').next().unwrap_or("");
    let stripped = last
        .strip_suffix(".tar.gz")
        .or_else(|| last.strip_suffix(".zip"))
        .or_else(|| last.strip_suffix(".tar.bz2"))
        .or_else(|| last.strip_suffix(".tar.xz"))
        .unwrap_or(last);
    if !stripped.is_empty() && stripped.chars().all(|c| c.is_ascii_hexdigit()) {
        version_string(stripped)
    } else {
        "0.0.0+override".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_with_sha_returns_override() {
        let line = "framework-arduino-lpc8xx@https://github.com/zackees/ArduinoCore-LPC8xx/archive/aaaabbbbccccddddeeeeffff0000111122223333.tar.gz#aaaabbbbccccddddeeeeffff0000111122223333";
        let got = parse_platform_packages_entry(line, "framework-arduino-lpc8xx").unwrap();
        assert_eq!(
            got.url,
            "https://github.com/zackees/ArduinoCore-LPC8xx/archive/aaaabbbbccccddddeeeeffff0000111122223333.tar.gz"
        );
        assert_eq!(got.version, "0.0.0+gaaaabbb");
        assert_eq!(got.checksum, None);
    }

    #[test]
    fn url_without_sha_derives_version_from_url_tail() {
        let line = "framework-arduino-lpc8xx@https://github.com/zackees/ArduinoCore-LPC8xx/archive/abcdef1234567890abcdef1234567890abcdef12.tar.gz";
        let got = parse_platform_packages_entry(line, "framework-arduino-lpc8xx").unwrap();
        assert_eq!(got.version, "0.0.0+gabcdef1");
    }

    #[test]
    fn owner_repo_with_sha_expands_to_github_archive() {
        let line = "framework-arduino-lpc8xx@zackees/ArduinoCore-LPC8xx#1234567890abcdef1234567890abcdef12345678";
        let got = parse_platform_packages_entry(line, "framework-arduino-lpc8xx").unwrap();
        assert_eq!(
            got.url,
            "https://github.com/zackees/ArduinoCore-LPC8xx/archive/1234567890abcdef1234567890abcdef12345678.tar.gz"
        );
        assert_eq!(got.version, "0.0.0+g1234567");
    }

    #[test]
    fn version_only_returns_none() {
        // `name @ 1.2.3` — registry version pin, not a URL override
        assert_eq!(
            parse_platform_packages_entry(
                "framework-arduino-lpc8xx @ 1.2.3",
                "framework-arduino-lpc8xx"
            ),
            None
        );
        // `name@1.2.3` (no space, no URL, no slash, no `#`)
        assert_eq!(
            parse_platform_packages_entry(
                "framework-arduino-lpc8xx@1.2.3",
                "framework-arduino-lpc8xx"
            ),
            None
        );
    }

    #[test]
    fn non_matching_package_name_returns_none() {
        let line = "some-other-package@https://example.com/archive/abc.tar.gz#abc";
        assert_eq!(
            parse_platform_packages_entry(line, "framework-arduino-lpc8xx"),
            None
        );
    }

    #[test]
    fn empty_or_blank_line_returns_none() {
        assert_eq!(parse_platform_packages_entry("", "x"), None);
        assert_eq!(parse_platform_packages_entry("   ", "x"), None);
    }

    #[test]
    fn multi_line_value_returns_first_match() {
        let value = "
            framework-other@https://example.com/archive/aaa.tar.gz#aaa
            framework-arduino-lpc8xx@https://github.com/zackees/ArduinoCore-LPC8xx/archive/deadbeefdeadbeefdeadbeefdeadbeefdeadbeef.tar.gz#deadbeefdeadbeefdeadbeefdeadbeefdeadbeef
            framework-arduino-lpc8xx@https://example.com/archive/should_not_win.tar.gz#1111111111111111111111111111111111111111
        ";
        let got = parse_platform_packages_value(value, "framework-arduino-lpc8xx").unwrap();
        assert_eq!(got.version, "0.0.0+gdeadbee");
        assert!(got.url.contains("ArduinoCore-LPC8xx"));
    }

    #[test]
    fn platform_release_zip_url_is_an_archive_pin() {
        let url = "https://github.com/pioarduino/platform-espressif32/releases/download/54.03.20/platform-espressif32.zip";
        let got = parse_platform_archive_url(url).unwrap();
        assert_eq!(got.url, url);
        assert_eq!(got.version, "54.03.20");
        assert_eq!(got.checksum, None);
    }

    #[test]
    fn platform_archive_tarball_url_is_an_archive_pin() {
        let url = "https://github.com/pioarduino/platform-espressif32/archive/abcdef1234567890abcdef1234567890abcdef12.tar.gz";
        let got = parse_platform_archive_url(url).unwrap();
        assert_eq!(got.url, url);
        assert_eq!(got.version, "0.0.0+gabcdef1");
    }

    #[test]
    fn platform_values_that_are_not_downloadable_archives_are_not_pins() {
        for value in [
            "",
            "espressif32",
            "espressif32@6.5.0",
            "https://github.com/pioarduino/platform-espressif32.git#develop",
            "https://github.com/pioarduino/platform-espressif32",
        ] {
            assert_eq!(parse_platform_archive_url(value), None, "{value:?}");
        }
    }

    #[test]
    fn release_download_url_in_platform_packages_uses_the_release_tag() {
        let line = "platform-espressif32@https://github.com/pioarduino/platform-espressif32/releases/download/55.03.35/platform-espressif32.zip";
        let got = parse_platform_packages_entry(line, "platform-espressif32").unwrap();
        assert_eq!(got.version, "55.03.35");
    }

    #[test]
    fn dot_release_tags_are_never_cache_versions() {
        // `.` and `..` would name the cache directory itself or its parent,
        // which could pass for an installed package and skip the download.
        for tag in [".", ".."] {
            let url = format!(
                "https://github.com/pioarduino/platform-espressif32/releases/download/{tag}/platform-espressif32.zip"
            );
            let got = parse_platform_archive_url(&url).unwrap();
            assert_eq!(got.version, "0.0.0+override", "{tag:?}");
        }
    }

    #[test]
    fn trailing_comma_or_semicolon_tolerated() {
        let line = "framework-arduino-lpc8xx@zackees/ArduinoCore-LPC8xx#abc,";
        let got = parse_platform_packages_entry(line, "framework-arduino-lpc8xx").unwrap();
        assert!(got.url.contains("archive/abc.tar.gz"));
    }

    fn env(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    /// FastLED/fbuild#1407: `platform = espressif32@6.5.0` used to resolve to
    /// the default platform with no word about the dropped version.
    #[test]
    fn platform_version_pin_is_reported_with_the_url_escape_hatch() {
        let warnings = ignored_version_pins(&env(&[("platform", "espressif32@6.5.0")]));
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("`6.5.0` is ignored"),
            "{}",
            warnings[0]
        );
        assert!(
            warnings[0].contains("platform-espressif32@<archive URL>"),
            "{}",
            warnings[0]
        );
    }

    #[test]
    fn owner_prefixed_spaced_platform_pin_is_reported() {
        let warnings = ignored_version_pins(&env(&[("platform", "platformio/atmelavr @ ~5.0.0")]));
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("`~5.0.0` is ignored"),
            "{}",
            warnings[0]
        );
        assert!(warnings[0].contains("<package>@<URL>"), "{}", warnings[0]);
    }

    #[test]
    fn bare_platform_packages_version_pin_is_reported() {
        let warnings = ignored_version_pins(&env(&[
            ("platform", "espressif32"),
            (
                "platform_packages",
                "platform-espressif32@6.5.0\nframework-arduinoespressif32@https://example.com/a.tar.gz",
            ),
        ]));
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("`platform_packages = platform-espressif32@6.5.0`"),
            "{}",
            warnings[0]
        );
    }

    #[test]
    fn honored_forms_produce_no_warning() {
        for (key, value) in [
            ("platform", "espressif32"),
            (
                "platform",
                "https://github.com/pioarduino/platform-espressif32/releases/download/54.03.20/platform-espressif32.zip",
            ),
            (
                "platform_packages",
                "platform-espressif32@https://github.com/pioarduino/platform-espressif32/archive/abc.tar.gz",
            ),
            (
                "platform_packages",
                "framework-arduino-lpc8xx@zackees/ArduinoCore-LPC8xx#abc",
            ),
        ] {
            let warnings = ignored_version_pins(&env(&[(key, value)]));
            assert!(warnings.is_empty(), "{key} = {value}: {warnings:?}");
        }
    }
}
