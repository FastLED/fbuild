//! Library specification parser for platformio.ini `lib_deps`.
//!
//! Supports formats:
//! - `owner/Name@^version`
//! - `Name@version`
//! - `https://github.com/owner/repo`
//! - `Name`

/// Parsed library dependency specification.
#[derive(Debug, Clone)]
pub struct LibrarySpec {
    pub owner: String,
    pub name: String,
    pub version: Option<String>,
    pub github_url: Option<String>,
}

impl LibrarySpec {
    /// Parse a library specification string from platformio.ini `lib_deps`.
    ///
    /// Supported formats:
    /// - `fastled/FastLED@^3.7.8` (owner/name@version)
    /// - `fastled/FastLED` (owner/name)
    /// - `FastLED@^3.7.8` (name@version, owner resolved via registry)
    /// - `FastLED` (name only)
    /// - `https://github.com/owner/repo` (GitHub URL)
    pub fn parse(spec: &str) -> Option<Self> {
        let spec = spec.trim();
        if spec.is_empty() {
            return None;
        }

        // Skip local paths (symlink://, file://, relative/absolute paths)
        if spec.starts_with("symlink://")
            || spec.starts_with("file://")
            || spec.starts_with("./")
            || spec.starts_with("../")
            || spec.starts_with('/')
            || spec.starts_with('\\')
            || (spec.len() > 2 && spec.as_bytes()[1] == b':')
        {
            return None;
        }

        // Handle Name=symlink://path (skip local libs)
        if spec.contains("symlink://") || spec.contains("file://") {
            return None;
        }

        // Handle GitHub URLs
        if spec.starts_with("http://") || spec.starts_with("https://") {
            if let Some((owner, name)) = parse_github_url(spec) {
                return Some(Self {
                    owner,
                    name,
                    version: None,
                    github_url: Some(spec.to_string()),
                });
            }
            return None;
        }

        // Parse owner/name@version format
        let (lib_part, version) = if let Some(at_pos) = spec.rfind('@') {
            let v = spec[at_pos + 1..].trim().to_string();
            let l = spec[..at_pos].trim();
            (l, Some(v))
        } else {
            (spec, None)
        };

        let lib_part = lib_part.trim();

        // Split owner/name
        if let Some(slash_pos) = lib_part.find('/') {
            let owner = lib_part[..slash_pos].trim().to_string();
            let name = lib_part[slash_pos + 1..].trim().to_string();
            Some(Self {
                owner,
                name,
                version,
                github_url: None,
            })
        } else {
            Some(Self {
                owner: String::new(),
                name: lib_part.to_string(),
                version,
                github_url: None,
            })
        }
    }

    /// Sanitize library name for filesystem use.
    pub fn sanitized_name(&self) -> String {
        self.name.to_lowercase().replace(['/', '@'], "_")
    }
}

/// Parse owner/name from a GitHub URL without using regex.
///
/// Handles: `https://github.com/owner/repo`, `https://github.com/owner/repo.git`
fn parse_github_url(url: &str) -> Option<(String, String)> {
    // Find "github.com/" in the URL
    let marker = "github.com/";
    let start = url.find(marker)? + marker.len();
    let rest = &url[start..];

    // Split remaining path by '/'
    let parts: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() < 2 {
        return None;
    }

    let owner = parts[0].to_string();
    let mut name = parts[1].to_string();

    // Strip .git suffix
    if let Some(stripped) = name.strip_suffix(".git") {
        name = stripped.to_string();
    }

    Some((owner, name))
}

impl std::fmt::Display for LibrarySpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if !self.owner.is_empty() {
            write!(f, "{}/{}", self.owner, self.name)?;
        } else {
            write!(f, "{}", self.name)?;
        }
        if let Some(ref v) = self.version {
            write!(f, "@{}", v)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_owner_name_version() {
        let spec = LibrarySpec::parse("fastled/FastLED@^3.7.8").unwrap();
        assert_eq!(spec.owner, "fastled");
        assert_eq!(spec.name, "FastLED");
        assert_eq!(spec.version, Some("^3.7.8".to_string()));
        assert!(spec.github_url.is_none());
    }

    #[test]
    fn test_parse_owner_name() {
        let spec = LibrarySpec::parse("fastled/FastLED").unwrap();
        assert_eq!(spec.owner, "fastled");
        assert_eq!(spec.name, "FastLED");
        assert!(spec.version.is_none());
    }

    #[test]
    fn test_parse_name_version() {
        let spec = LibrarySpec::parse("ArduinoJson@^7.0.0").unwrap();
        assert_eq!(spec.owner, "");
        assert_eq!(spec.name, "ArduinoJson");
        assert_eq!(spec.version, Some("^7.0.0".to_string()));
    }

    #[test]
    fn test_parse_name_only() {
        let spec = LibrarySpec::parse("FastLED").unwrap();
        assert_eq!(spec.owner, "");
        assert_eq!(spec.name, "FastLED");
        assert!(spec.version.is_none());
    }

    #[test]
    fn test_parse_github_url() {
        let spec = LibrarySpec::parse("https://github.com/FastLED/FastLED").unwrap();
        assert_eq!(spec.owner, "FastLED");
        assert_eq!(spec.name, "FastLED");
        assert!(spec.github_url.is_some());
    }

    #[test]
    fn test_parse_github_url_with_git() {
        let spec =
            LibrarySpec::parse("https://github.com/me-no-dev/ESPAsyncWebServer.git").unwrap();
        assert_eq!(spec.owner, "me-no-dev");
        assert_eq!(spec.name, "ESPAsyncWebServer");
    }

    #[test]
    fn test_parse_empty() {
        assert!(LibrarySpec::parse("").is_none());
        assert!(LibrarySpec::parse("   ").is_none());
    }

    #[test]
    fn test_parse_skip_symlink() {
        assert!(LibrarySpec::parse("symlink://./").is_none());
        assert!(LibrarySpec::parse("FastLED=symlink://./").is_none());
    }

    #[test]
    fn test_parse_skip_file() {
        assert!(LibrarySpec::parse("file://../../fastled9").is_none());
    }

    #[test]
    fn test_parse_skip_relative_path() {
        assert!(LibrarySpec::parse("../../fastled9").is_none());
        assert!(LibrarySpec::parse("./local_lib").is_none());
    }

    #[test]
    fn test_sanitized_name() {
        let spec = LibrarySpec::parse("FastLED").unwrap();
        assert_eq!(spec.sanitized_name(), "fastled");
    }

    #[test]
    fn test_display() {
        let spec = LibrarySpec::parse("fastled/FastLED@^3.7.8").unwrap();
        assert_eq!(format!("{}", spec), "fastled/FastLED@^3.7.8");

        let spec = LibrarySpec::parse("FastLED").unwrap();
        assert_eq!(format!("{}", spec), "FastLED");
    }

    #[test]
    fn test_nightdriverstrip_deps() {
        // All NightDriverStrip demo lib_deps formats
        let specs = vec![
            "fastled/FastLED@^3.7.8",
            "bblanchon/ArduinoJson@^7.0.0",
            "https://github.com/me-no-dev/ESPAsyncWebServer.git",
            "https://github.com/me-no-dev/AsyncTCP.git",
            "WiFi",
            "FS",
            "SPIFFS",
        ];

        for s in specs {
            let parsed = LibrarySpec::parse(s);
            assert!(parsed.is_some(), "failed to parse: {}", s);
            let parsed = parsed.unwrap();
            assert!(!parsed.name.is_empty(), "empty name for: {}", s);
        }
    }
}
