//! Library specification parser for platformio.ini `lib_deps`.
//!
//! Supports formats:
//! - `owner/Name@^version`
//! - `Name@version`
//! - `https://github.com/owner/repo`
//! - `Name`
//! - `Name=symlink://path` / `Name=file://path` (named local library)
//! - `symlink://path`, `file://path`, or a bare directory path (unnamed
//!   local library; name read from its manifest)

/// Parsed library dependency specification.
#[derive(Debug, Clone)]
pub struct LibrarySpec {
    pub owner: String,
    pub name: String,
    pub version: Option<String>,
    pub github_url: Option<String>,
    /// Local library root from a `Name=symlink://path` or `Name=file://path` dependency.
    pub local_path: Option<fbuild_core::path::NormalizedPath>,
    /// True for an unnamed local dependency (`symlink://path`, `file://path`
    /// or a bare directory path). `name` then holds the directory-name
    /// fallback, and the library manager replaces it with the `name` from the
    /// library's `library.json` / `library.properties` once the path is
    /// resolved against the project.
    pub infer_name: bool,
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

        // Named local dependency: `Name=symlink://path` / `Name=file://path`.
        // Keep the dependency name so `lib_ignore` and the build output
        // directory remain deterministic.
        for sep in ["=symlink://", "=file://"] {
            if let Some((name, path)) = spec.split_once(sep) {
                let name = name.trim();
                let path = strip_file_uri_drive_slash(path.trim());
                if name.is_empty() || path.is_empty() {
                    return None;
                }
                return Some(Self::local(name.to_string(), path, false));
            }
        }

        // Unnamed local dependency, as PlatformIO accepts it:
        // `symlink://path`, `file://path`, or a bare absolute/relative
        // directory path. The library is used in place (like `lib/<name>`),
        // and its name comes from its manifest, falling back to the
        // directory name.
        if let Some(path) = local_path_of(spec) {
            if path.is_empty() {
                return None;
            }
            let name = dir_name_fallback(path);
            return Some(Self::local(name, path, true));
        }

        // Handle GitHub URLs
        if spec.starts_with("http://") || spec.starts_with("https://") {
            if let Some((owner, name)) = parse_github_url(spec) {
                return Some(Self {
                    owner,
                    name,
                    version: None,
                    github_url: Some(spec.to_string()),
                    local_path: None,
                    infer_name: false,
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
                local_path: None,
                infer_name: false,
            })
        } else {
            Some(Self {
                owner: String::new(),
                name: lib_part.to_string(),
                version,
                github_url: None,
                local_path: None,
                infer_name: false,
            })
        }
    }

    fn local(name: String, path: &str, infer_name: bool) -> Self {
        Self {
            owner: String::new(),
            name,
            version: None,
            github_url: None,
            local_path: Some(fbuild_core::path::NormalizedPath::new(path)),
            infer_name,
        }
    }

    /// Sanitize library name for filesystem use.
    pub fn sanitized_name(&self) -> String {
        self.name.to_lowercase().replace(['/', '@', ' '], "_")
    }
}

/// Return the path of an unnamed local `lib_deps` entry, or `None` when the
/// spec is not a local path.
fn local_path_of(spec: &str) -> Option<&str> {
    for scheme in ["symlink://", "file://"] {
        if let Some(rest) = spec.strip_prefix(scheme) {
            return Some(strip_file_uri_drive_slash(rest.trim()));
        }
    }
    let is_bare_path = spec.starts_with("./")
        || spec.starts_with("../")
        || spec == "."
        || spec == ".."
        || spec.starts_with(".\\")
        || spec.starts_with("..\\")
        || spec.starts_with('/')
        || spec.starts_with('\\')
        || (spec.len() > 2
            && spec.as_bytes()[1] == b':'
            && spec.as_bytes()[0].is_ascii_alphabetic());
    is_bare_path.then_some(spec)
}

/// `file:///C:/path` is the standard Windows spelling: the URI form keeps a
/// leading slash, but Windows needs the drive prefix.
fn strip_file_uri_drive_slash(path: &str) -> &str {
    if fbuild_core::platform::host::is_windows()
        && path.len() > 2
        && path.as_bytes()[0] == b'/'
        && path.as_bytes()[2] == b':'
    {
        &path[1..]
    } else {
        path
    }
}

/// Last non-trivial path component, used as the name of an unnamed local
/// library until its manifest is read.
fn dir_name_fallback(path: &str) -> String {
    path.split(['/', '\\'])
        .rfind(|c| !c.is_empty() && *c != "." && *c != "..")
        .unwrap_or(path)
        .to_string()
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
    fn test_parse_unnamed_symlink_absolute() {
        let spec = LibrarySpec::parse("symlink:///home/me/dev/fastled2").unwrap();
        assert_eq!(spec.name, "fastled2");
        assert!(spec.infer_name);
        assert_eq!(
            spec.local_path,
            Some(fbuild_core::path::NormalizedPath::new(
                "/home/me/dev/fastled2"
            ))
        );
    }

    #[test]
    fn test_parse_unnamed_symlink_relative() {
        let spec = LibrarySpec::parse("symlink://../libs/MyLib/").unwrap();
        assert_eq!(spec.name, "MyLib");
        assert!(spec.infer_name);
        assert_eq!(
            spec.local_path,
            Some(fbuild_core::path::NormalizedPath::new("../libs/MyLib/"))
        );
        // `symlink://./` is the project itself: still a local spec, never dropped.
        assert!(LibrarySpec::parse("symlink://./").unwrap().infer_name);
    }

    #[test]
    fn test_parse_named_symlink() {
        let spec = LibrarySpec::parse("FastLED=symlink://../..").unwrap();
        assert_eq!(spec.name, "FastLED");
        assert_eq!(
            spec.local_path,
            Some(fbuild_core::path::NormalizedPath::new("../.."))
        );
    }

    #[test]
    fn test_parse_named_relative_file_uri() {
        let spec = LibrarySpec::parse("FastLED=file://../..").unwrap();
        assert_eq!(
            spec.local_path,
            Some(fbuild_core::path::NormalizedPath::new("../.."))
        );
    }

    #[test]
    fn test_parse_named_windows_file_uri() {
        // The `file:///C:/...` drive-prefix strip is a Windows-host behavior
        // (on POSIX, `/C:/...` is a legitimate absolute path and stays).
        let spec = LibrarySpec::parse("FastLED=file:///C:/libraries/FastLED").unwrap();
        let expected = if fbuild_core::platform::host::is_windows() {
            "C:/libraries/FastLED"
        } else {
            "/C:/libraries/FastLED"
        };
        assert_eq!(
            spec.local_path,
            Some(fbuild_core::path::NormalizedPath::new(expected))
        );
    }

    #[test]
    fn test_parse_unnamed_file_uri() {
        let spec = LibrarySpec::parse("file://../../fastled9").unwrap();
        assert_eq!(spec.name, "fastled9");
        assert!(spec.infer_name);
        assert_eq!(
            spec.local_path,
            Some(fbuild_core::path::NormalizedPath::new("../../fastled9"))
        );
    }

    #[test]
    fn test_parse_bare_paths() {
        for (raw, name) in [
            ("../../fastled9", "fastled9"),
            ("./local_lib", "local_lib"),
            ("/home/me/dev/fastled2", "fastled2"),
        ] {
            let spec = LibrarySpec::parse(raw).unwrap();
            assert_eq!(spec.name, name, "{raw}");
            assert!(spec.infer_name, "{raw}");
            assert_eq!(
                spec.local_path,
                Some(fbuild_core::path::NormalizedPath::new(raw))
            );
        }
    }

    #[test]
    fn test_parse_named_local_does_not_infer() {
        assert!(
            !LibrarySpec::parse("FastLED=symlink://../..")
                .unwrap()
                .infer_name
        );
    }

    #[test]
    fn test_parse_registry_spec_is_not_local() {
        let spec = LibrarySpec::parse("fastled/FastLED").unwrap();
        assert!(spec.local_path.is_none());
        assert!(!spec.infer_name);
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
