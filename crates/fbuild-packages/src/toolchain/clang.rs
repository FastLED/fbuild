//! Clang toolchain component management.
//!
//! Downloads and caches LLVM-based tools from clang-tool-chain-bins:
//! - **Clang**: full LLVM toolchain (clang, clang++, lld, llvm-ar, etc.)
//! - **ClangExtra**: analysis tools (clang-tidy, clang-format, clang-query)
//! - **Iwyu**: include-what-you-use

use std::path::{Path, PathBuf};

use crate::{CacheSubdir, PackageBase};

const MANIFEST_BASE: &str = "https://zackees.github.io/clang-tool-chain-bins/assets";

/// Which clang-tool-chain-bins component to install.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClangComponentKind {
    /// Full LLVM toolchain: clang, clang++, lld, llvm-ar, etc.
    Clang,
    /// Extra analysis tools: clang-tidy, clang-format, clang-query.
    ClangExtra,
    /// include-what-you-use.
    Iwyu,
}

impl ClangComponentKind {
    /// Component name as it appears in the manifest URL path.
    pub fn component_name(&self) -> &'static str {
        match self {
            Self::Clang => "clang",
            Self::ClangExtra => "clang-extra",
            Self::Iwyu => "iwyu",
        }
    }

    /// Binaries that must exist after extraction (validation).
    pub fn required_binaries(&self) -> &'static [&'static str] {
        match self {
            Self::Clang => &["clang", "clang++", "lld"],
            Self::ClangExtra => &["clang-tidy"],
            Self::Iwyu => &["include-what-you-use"],
        }
    }
}

/// Parsed platform manifest entry.
#[derive(Debug, Clone)]
struct ManifestEntry {
    version: String,
    href: String,
    sha256: String,
}

/// Manages a single clang-tool-chain-bins component.
///
/// Fetches manifest from GitHub Pages, downloads `.tar.zst` on demand,
/// validates required binaries, caches in `~/.fbuild/{dev|prod}/cache/toolchains/`.
pub struct ClangComponent {
    kind: ClangComponentKind,
}

impl ClangComponent {
    pub fn new(kind: ClangComponentKind) -> Self {
        Self { kind }
    }

    /// Ensure the component is installed, downloading on first use.
    /// Returns the install directory containing the extracted archive.
    pub async fn ensure_installed(&self) -> fbuild_core::Result<PathBuf> {
        // Fast path: already cached
        if let Some(cached) = self.find_cached_version() {
            return Ok(cached);
        }

        // Fetch manifest, with offline fallback
        let manifest = match self.fetch_manifest().await {
            Ok(m) => m,
            Err(e) => {
                if let Some(cached) = self.find_cached_version() {
                    tracing::warn!(
                        "manifest fetch failed ({}), using cached {}",
                        e,
                        self.kind.component_name()
                    );
                    return Ok(cached);
                }
                return Err(e);
            }
        };

        // Check if this specific version is already cached
        let package = self.make_package(&manifest);
        if package.is_cached() {
            return Ok(package.install_path());
        }

        // Download, extract, validate
        let kind = self.kind;
        let install_path = package
            .staged_install(move |dir| Self::validate(kind, dir))
            .await?;

        Ok(install_path)
    }

    /// Get path to a specific binary (e.g., "clang-tidy").
    /// Calls `ensure_installed()` internally.
    pub async fn get_binary(&self, name: &str) -> fbuild_core::Result<PathBuf> {
        let install_dir = self.ensure_installed().await?;
        let binary_name = if cfg!(windows) {
            format!("{}.exe", name)
        } else {
            name.to_string()
        };
        find_binary_in_dir(&install_dir, &binary_name).ok_or_else(|| {
            fbuild_core::FbuildError::Other(format!(
                "'{}' not found in {} installation at {}",
                name,
                self.kind.component_name(),
                install_dir.display()
            ))
        })
    }

    /// Check if any version is cached locally.
    pub fn is_installed(&self) -> bool {
        self.find_cached_version().is_some()
    }

    // --- Private ---

    fn cache_key(&self) -> String {
        format!("{}/{}", MANIFEST_BASE, self.kind.component_name())
    }

    fn manifest_url(&self) -> String {
        format!(
            "{}/{}/{}/{}/manifest.json",
            MANIFEST_BASE,
            self.kind.component_name(),
            platform(),
            arch()
        )
    }

    async fn fetch_manifest(&self) -> fbuild_core::Result<ManifestEntry> {
        let url = self.manifest_url();
        let resp = reqwest::get(&url).await.map_err(|e| {
            fbuild_core::FbuildError::Other(format!(
                "failed to fetch {} manifest from {}: {}",
                self.kind.component_name(),
                url,
                e
            ))
        })?;
        if !resp.status().is_success() {
            return Err(fbuild_core::FbuildError::Other(format!(
                "{} manifest returned HTTP {}: {}",
                self.kind.component_name(),
                resp.status(),
                url
            )));
        }
        let body: serde_json::Value = resp.json().await.map_err(|e| {
            fbuild_core::FbuildError::Other(format!(
                "failed to parse {} manifest: {}",
                self.kind.component_name(),
                e
            ))
        })?;
        let version = body["latest"]
            .as_str()
            .ok_or_else(|| {
                fbuild_core::FbuildError::Other(format!(
                    "no 'latest' key in {} manifest",
                    self.kind.component_name()
                ))
            })?
            .to_string();
        let entry = &body[&version];
        let href = entry["href"]
            .as_str()
            .ok_or_else(|| {
                fbuild_core::FbuildError::Other(format!(
                    "no 'href' for version {} in {} manifest",
                    version,
                    self.kind.component_name()
                ))
            })?
            .to_string();
        let sha256 = entry["sha256"]
            .as_str()
            .ok_or_else(|| {
                fbuild_core::FbuildError::Other(format!(
                    "no 'sha256' for version {} in {} manifest",
                    version,
                    self.kind.component_name()
                ))
            })?
            .to_string();
        Ok(ManifestEntry {
            version,
            href,
            sha256,
        })
    }

    fn make_package(&self, manifest: &ManifestEntry) -> PackageBase {
        PackageBase::new(
            self.kind.component_name(),
            &manifest.version,
            &manifest.href,
            &self.cache_key(),
            Some(&manifest.sha256),
            CacheSubdir::Toolchains,
            Path::new("."),
        )
    }

    fn find_cached_version(&self) -> Option<PathBuf> {
        let cache = crate::Cache::new(Path::new("."));
        // The cache path is: {root}/toolchains/{stem}/{hash}/
        // We need to check if any version directory exists under the hash dir.
        let cache_key = self.cache_key();

        // Try "latest" first (common case after first install)
        let latest_path = cache.get_toolchain_path(&cache_key, "latest");
        if latest_path.exists() && latest_path.is_dir() {
            return Some(latest_path);
        }

        // Walk the hash directory for any version
        if let Some(hash_dir) = latest_path.parent() {
            if let Ok(entries) = std::fs::read_dir(hash_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let p = entry.path();
                    if p.is_dir() && !p.to_string_lossy().ends_with("_staging") {
                        return Some(p);
                    }
                }
            }
        }
        None
    }

    fn validate(kind: ClangComponentKind, dir: &Path) -> fbuild_core::Result<()> {
        for name in kind.required_binaries() {
            let binary_name = if cfg!(windows) {
                format!("{}.exe", name)
            } else {
                (*name).to_string()
            };
            if find_binary_in_dir(dir, &binary_name).is_none() {
                return Err(fbuild_core::FbuildError::PackageError(format!(
                    "'{}' not found in extracted {} archive",
                    name,
                    kind.component_name()
                )));
            }
        }
        Ok(())
    }
}

/// Recursively search for a binary by name in a directory.
/// Checks `dir/bin/name`, then `dir/*/bin/name` (one level of nesting).
pub fn find_binary_in_dir(dir: &Path, name: &str) -> Option<PathBuf> {
    if !dir.exists() {
        return None;
    }
    // Direct: dir/bin/name
    let direct = dir.join("bin").join(name);
    if direct.exists() {
        return Some(direct);
    }
    // One level nested: dir/subdir/bin/name (archives often have a top-level folder)
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let p = entry.path();
            if p.is_dir() {
                let candidate = p.join("bin").join(name);
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

fn platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "win"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    }
}

fn arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x86_64"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_component_names() {
        assert_eq!(ClangComponentKind::Clang.component_name(), "clang");
        assert_eq!(
            ClangComponentKind::ClangExtra.component_name(),
            "clang-extra"
        );
        assert_eq!(ClangComponentKind::Iwyu.component_name(), "iwyu");
    }

    #[test]
    fn test_manifest_url_construction() {
        let c = ClangComponent::new(ClangComponentKind::ClangExtra);
        let url = c.manifest_url();
        assert!(url.starts_with(MANIFEST_BASE));
        assert!(url.contains("clang-extra"));
        assert!(url.ends_with("/manifest.json"));
    }

    #[test]
    fn test_required_binaries() {
        assert!(ClangComponentKind::ClangExtra
            .required_binaries()
            .contains(&"clang-tidy"));
        assert!(ClangComponentKind::Iwyu
            .required_binaries()
            .contains(&"include-what-you-use"));
        assert!(ClangComponentKind::Clang
            .required_binaries()
            .contains(&"clang"));
    }

    #[test]
    fn test_validate_missing_binary() {
        let dir = tempfile::tempdir().unwrap();
        let result = ClangComponent::validate(ClangComponentKind::ClangExtra, dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_present_binary() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let name = if cfg!(windows) {
            "clang-tidy.exe"
        } else {
            "clang-tidy"
        };
        std::fs::write(bin_dir.join(name), b"fake").unwrap();
        let result = ClangComponent::validate(ClangComponentKind::ClangExtra, dir.path());
        assert!(result.is_ok());
    }

    #[test]
    fn test_find_binary_nested() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("llvm-21.1.5").join("bin");
        std::fs::create_dir_all(&nested).unwrap();
        let name = if cfg!(windows) {
            "clang-tidy.exe"
        } else {
            "clang-tidy"
        };
        std::fs::write(nested.join(name), b"fake").unwrap();
        assert!(find_binary_in_dir(dir.path(), name).is_some());
    }

    #[test]
    fn test_find_binary_not_found() {
        let dir = tempfile::tempdir().unwrap();
        assert!(find_binary_in_dir(dir.path(), "clang-tidy").is_none());
    }
}
