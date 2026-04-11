//! Sole source of cache path construction for the two-phase disk cache.
//!
//! All cache paths flow through this module. No other code in the workspace
//! should construct cache paths directly.

use crate::cache::{hash_url, url_stem};
use std::path::{Path, PathBuf};

/// The kind of cached artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Packages,
    Toolchains,
    Platforms,
    Libraries,
    Frameworks,
}

impl Kind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Packages => "packages",
            Kind::Toolchains => "toolchains",
            Kind::Platforms => "platforms",
            Kind::Libraries => "libraries",
            Kind::Frameworks => "frameworks",
        }
    }

    pub fn all() -> &'static [Kind] {
        &[
            Kind::Packages,
            Kind::Toolchains,
            Kind::Platforms,
            Kind::Libraries,
            Kind::Frameworks,
        ]
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Kind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "packages" => Ok(Kind::Packages),
            "toolchains" => Ok(Kind::Toolchains),
            "platforms" => Ok(Kind::Platforms),
            "libraries" => Ok(Kind::Libraries),
            "frameworks" => Ok(Kind::Frameworks),
            other => Err(format!("unknown cache kind: {}", other)),
        }
    }
}

/// Compute the stem and hash for a URL (delegates to existing helpers).
pub fn stem_and_hash(url: &str) -> (String, String) {
    (url_stem(url), hash_url(url))
}

/// Sanitize a path component to prevent directory traversal.
/// Strips path separators, `.` and `..` sequences, and null bytes.
/// Returns `"_"` if the result would be empty or `"."`.
///
/// When the sanitized form differs from the input (lossy transformation)
/// or would collide with staging `.partial` directories, a short hash of
/// the original string is appended to keep the mapping collision-free.
fn sanitize_component(s: &str) -> String {
    let sanitized = s.replace(['/', '\\', '\0'], "_").replace("..", "_");
    let sanitized = if sanitized.is_empty() || sanitized == "." {
        "_".to_string()
    } else {
        sanitized
    };
    // Append a short hash when the mapping is lossy or the name would collide
    // with the `.partial` staging convention.
    if sanitized != s || sanitized.ends_with(".partial") {
        let hash = short_hash(s);
        format!("{}_{}", sanitized, hash)
    } else {
        sanitized
    }
}

/// 8-char hex hash of a string, for disambiguation suffixes.
fn short_hash(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    let result = hasher.finalize();
    // First 4 bytes → 8 hex chars
    result[..4]
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>()
}

/// Root of the archives phase: `{cache_root}/archives/`
pub fn archives_root(cache_root: &Path) -> PathBuf {
    cache_root.join("archives")
}

/// Root of the installed phase: `{cache_root}/installed/`
pub fn installed_root(cache_root: &Path) -> PathBuf {
    cache_root.join("installed")
}

/// Path to the SQLite index: `{cache_root}/index.sqlite`
pub fn index_path(cache_root: &Path) -> PathBuf {
    cache_root.join("index.sqlite")
}

/// Archive entry path: `{cache_root}/archives/{kind}/{stem}/{hash}/{version}/`
pub fn archive_entry_dir(cache_root: &Path, kind: Kind, url: &str, version: &str) -> PathBuf {
    let (stem, hash) = stem_and_hash(url);
    let safe_version = sanitize_component(version);
    archives_root(cache_root)
        .join(kind.as_str())
        .join(stem)
        .join(hash)
        .join(safe_version)
}

/// Staging path for an in-progress archive download.
/// Uses `.partial` suffix so reconciliation can identify incomplete downloads.
pub fn archive_staging_dir(cache_root: &Path, kind: Kind, url: &str, version: &str) -> PathBuf {
    let (stem, hash) = stem_and_hash(url);
    let safe_version = sanitize_component(version);
    archives_root(cache_root)
        .join(kind.as_str())
        .join(stem)
        .join(hash)
        .join(format!("{}.partial", safe_version))
}

/// Installed entry path: `{cache_root}/installed/{kind}/{stem}/{hash}/{version}/`
pub fn installed_entry_dir(cache_root: &Path, kind: Kind, url: &str, version: &str) -> PathBuf {
    let (stem, hash) = stem_and_hash(url);
    let safe_version = sanitize_component(version);
    installed_root(cache_root)
        .join(kind.as_str())
        .join(stem)
        .join(hash)
        .join(safe_version)
}

/// Staging path for an in-progress extraction.
pub fn install_staging_dir(cache_root: &Path, kind: Kind, url: &str, version: &str) -> PathBuf {
    let (stem, hash) = stem_and_hash(url);
    let safe_version = sanitize_component(version);
    installed_root(cache_root)
        .join(kind.as_str())
        .join(stem)
        .join(hash)
        .join(format!("{}.partial", safe_version))
}

/// Sentinel file that marks a completed installation.
pub fn install_complete_sentinel(installed_dir: &Path) -> PathBuf {
    installed_dir.join(".install_complete")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_archives_root() {
        let root = Path::new("/tmp/cache");
        assert_eq!(archives_root(root), Path::new("/tmp/cache/archives"));
    }

    #[test]
    fn test_installed_root() {
        let root = Path::new("/tmp/cache");
        assert_eq!(installed_root(root), Path::new("/tmp/cache/installed"));
    }

    #[test]
    fn test_index_path() {
        let root = Path::new("/tmp/cache");
        assert_eq!(index_path(root), Path::new("/tmp/cache/index.sqlite"));
    }

    #[test]
    fn test_archive_entry_dir_structure() {
        let root = Path::new("/tmp/cache");
        let url = "https://github.com/FastLED/FastLED#master";
        let dir = archive_entry_dir(root, Kind::Libraries, url, "3.6.0");
        let dir_str = dir.to_string_lossy();
        assert!(dir_str.contains("archives"));
        assert!(dir_str.contains("libraries"));
        assert!(dir_str.contains("FastLED-FastLED")); // stem
        assert!(dir_str.contains("3.6.0")); // version
    }

    #[test]
    fn test_installed_entry_dir_structure() {
        let root = Path::new("/tmp/cache");
        let url = "https://downloads.arduino.cc/tools";
        let dir = installed_entry_dir(root, Kind::Toolchains, url, "7.3.0");
        let dir_str = dir.to_string_lossy();
        assert!(dir_str.contains("installed"));
        assert!(dir_str.contains("toolchains"));
        assert!(dir_str.contains("arduino-tools")); // stem
        assert!(dir_str.contains("7.3.0"));
    }

    #[test]
    fn test_staging_dirs_have_partial_suffix() {
        let root = Path::new("/tmp/cache");
        let url = "https://example.com/pkg.tar.gz";
        let archive_staging = archive_staging_dir(root, Kind::Packages, url, "1.0.0");
        assert!(archive_staging.to_string_lossy().ends_with("1.0.0.partial"));

        let install_staging = install_staging_dir(root, Kind::Packages, url, "1.0.0");
        assert!(install_staging.to_string_lossy().ends_with("1.0.0.partial"));
    }

    #[test]
    fn test_install_complete_sentinel() {
        let dir = Path::new("/tmp/cache/installed/toolchains/gcc/abc123/7.3.0");
        let sentinel = install_complete_sentinel(dir);
        assert_eq!(
            sentinel,
            Path::new("/tmp/cache/installed/toolchains/gcc/abc123/7.3.0/.install_complete")
        );
    }

    #[test]
    fn test_kind_roundtrip() {
        for kind in Kind::all() {
            let s = kind.as_str();
            let parsed: Kind = s.parse().unwrap();
            assert_eq!(*kind, parsed);
        }
    }

    #[test]
    fn test_kind_display() {
        assert_eq!(format!("{}", Kind::Toolchains), "toolchains");
        assert_eq!(format!("{}", Kind::Packages), "packages");
    }

    #[test]
    fn test_stem_and_hash_delegates_correctly() {
        let url = "https://example.com/pkg.tar.gz";
        let (stem, hash) = stem_and_hash(url);
        assert_eq!(stem, url_stem(url));
        assert_eq!(hash, hash_url(url));
    }

    #[test]
    fn test_version_traversal_sanitized() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        let url = "https://example.com/pkg";

        // Path traversal attempt should be sanitized
        let dir = archive_entry_dir(root, Kind::Packages, url, "../../etc/passwd");
        let dir_str = dir.to_string_lossy();
        assert!(
            !dir_str.contains(".."),
            "path traversal not sanitized: {}",
            dir_str
        );
        // Ensure the path stays under the cache root
        assert!(
            dir.starts_with(root),
            "path escaped cache root: {}",
            dir_str
        );

        // Backslash traversal
        let dir = installed_entry_dir(root, Kind::Packages, url, r"1.0\..\..\etc");
        let dir_str = dir.to_string_lossy();
        assert!(
            !dir_str.contains(".."),
            "backslash traversal not sanitized: {}",
            dir_str
        );

        // Normal versions are unchanged (no hash suffix)
        let dir = archive_entry_dir(root, Kind::Packages, url, "1.2.3");
        assert!(dir.to_string_lossy().ends_with("1.2.3"));
    }

    #[test]
    fn test_sanitize_collision_free() {
        // Different inputs that would collide without hash suffix
        let a = sanitize_component("1/2");
        let b = sanitize_component("1_2");
        assert_ne!(a, b, "lossy sanitization must be disambiguated");

        // Normal version: no suffix
        let c = sanitize_component("1.2.3");
        assert_eq!(c, "1.2.3");
    }

    #[test]
    fn test_sanitize_partial_collision() {
        // "1.partial" must not collide with staging dir for version "1"
        let a = sanitize_component("1.partial");
        assert!(
            !a.ends_with(".partial"),
            "must not end with .partial: {}",
            a
        );
    }
}
