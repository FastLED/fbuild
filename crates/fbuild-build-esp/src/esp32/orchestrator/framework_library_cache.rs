//! Content-addressed cache for ESP32 framework-supplied library archives.
//!
//! Framework libraries are not project outputs. A normal clean removes the
//! project's build directory, then hydrates these archives again; `--clean-all`
//! explicitly evicts this cache entry.

use std::path::{Path, PathBuf};

use fbuild_core::BuildProfile;
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

const CACHE_VERSION: &str = "fbuild-esp32-framework-libraries-v1";

pub(super) struct FrameworkLibraryCache {
    path: PathBuf,
}

impl FrameworkLibraryCache {
    pub(super) fn new(
        project_dir: &Path,
        profile: BuildProfile,
        compile_signature: &str,
        libraries_dir: &Path,
    ) -> Self {
        let key = cache_key(project_dir, profile, compile_signature, libraries_dir);
        let path = fbuild_packages::Cache::new(project_dir)
            .framework_library_artifacts_dir()
            .join(key);
        Self { path }
    }

    #[cfg(test)]
    fn with_cache_root(
        project_dir: &Path,
        cache_root: &Path,
        profile: BuildProfile,
        compile_signature: &str,
        libraries_dir: &Path,
    ) -> Self {
        let key = cache_key(project_dir, profile, compile_signature, libraries_dir);
        let path = fbuild_packages::Cache::with_cache_root(project_dir, cache_root)
            .framework_library_artifacts_dir()
            .join(key);
        Self { path }
    }

    pub(super) fn hydrate(&self, target_dir: &Path) -> std::io::Result<usize> {
        if !self.path.is_dir() {
            return Ok(0);
        }
        std::fs::create_dir_all(target_dir)?;
        let mut copied = 0;
        for entry in std::fs::read_dir(&self.path)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() || !is_archive(&entry.path()) {
                continue;
            }
            let destination = target_dir.join(entry.file_name());
            if destination.exists() {
                continue;
            }
            std::fs::copy(entry.path(), destination)?;
            copied += 1;
        }
        Ok(copied)
    }

    pub(super) fn store_archive(&self, archive: &Path) -> std::io::Result<()> {
        let Some(name) = archive.file_name() else {
            return Ok(());
        };
        if !is_archive(archive) {
            return Ok(());
        }
        std::fs::create_dir_all(&self.path)?;
        let destination = self.path.join(name);
        if !destination.exists() {
            std::fs::copy(archive, destination)?;
        }
        Ok(())
    }

    pub(super) fn has_failed(&self, library_name: &str) -> bool {
        self.failure_path(library_name).is_file()
    }

    pub(super) fn record_failure(&self, library_name: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(self.path.join("failed"))?;
        std::fs::write(self.failure_path(library_name), b"failed\n")
    }

    pub(super) fn remove(&self) -> std::io::Result<()> {
        if self.path.exists() {
            std::fs::remove_dir_all(&self.path)?;
        }
        Ok(())
    }

    fn failure_path(&self, library_name: &str) -> PathBuf {
        self.path.join("failed").join(library_name)
    }
}

fn cache_key(
    project_dir: &Path,
    profile: BuildProfile,
    compile_signature: &str,
    libraries_dir: &Path,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(CACHE_VERSION.as_bytes());
    hasher.update([0]);
    hasher.update(env!("CARGO_PKG_VERSION").as_bytes());
    hasher.update([0]);
    hasher.update(profile.as_dir_name().as_bytes());
    hasher.update([0]);
    hasher.update(normalize_compile_signature(project_dir, compile_signature).as_bytes());
    hasher.update([0]);
    hash_tree(&mut hasher, libraries_dir, false);
    hash_project_headers(&mut hasher, project_dir);
    format!("{:x}", hasher.finalize())
}

/// Library compilation receives project include directories. Replace the
/// machine-specific project prefix so identical projects share the global
/// cache, while preserving all framework/toolchain paths and flags.
fn normalize_compile_signature(project_dir: &Path, signature: &str) -> String {
    let raw_project = project_dir.to_string_lossy();
    let normalized_project = fbuild_core::path::NormalizedPath::new(project_dir).display_slash();
    signature
        .replace(raw_project.as_ref(), "$PROJECT")
        .replace(&normalized_project, "$PROJECT")
}

fn hash_tree(hasher: &mut Sha256, root: &Path, headers_only: bool) {
    if !root.is_dir() {
        hasher.update(b"missing");
        return;
    }
    let mut files: Vec<PathBuf> = WalkDir::new(root)
        .into_iter()
        .flatten()
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .filter(|path| !headers_only || is_header(path))
        .collect();
    files.sort();
    for path in files {
        let relative = path.strip_prefix(root).unwrap_or(&path);
        hasher.update(relative.to_string_lossy().replace('\\', "/").as_bytes());
        hasher.update([0]);
        match std::fs::read(&path) {
            Ok(bytes) => hasher.update(bytes),
            Err(error) => hasher.update(format!("unreadable:{:?}", error.kind()).as_bytes()),
        }
        hasher.update([0xff]);
    }
}

fn hash_project_headers(hasher: &mut Sha256, project_dir: &Path) {
    for name in ["src", "include", "lib"] {
        hasher.update(name.as_bytes());
        hasher.update([0]);
        hash_tree(hasher, &project_dir.join(name), true);
    }
}

fn is_archive(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("a")
}

fn is_header(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("h" | "hh" | "hpp" | "hxx")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_is_independent_of_project_path_when_headers_match() {
        let tmp = tempfile::tempdir().unwrap();
        let framework = tmp.path().join("framework");
        std::fs::create_dir_all(framework.join("WiFi/src")).unwrap();
        std::fs::write(framework.join("WiFi/src/WiFi.cpp"), "int wifi;").unwrap();
        let project_a = tmp.path().join("project-a");
        let project_b = tmp.path().join("project-b");
        for project in [&project_a, &project_b] {
            std::fs::create_dir_all(project.join("include")).unwrap();
            std::fs::write(project.join("include/config.h"), "#define WIFI 1").unwrap();
        }

        let a_signature = format!("-I{}", project_a.join("include").display());
        let b_signature = format!("-I{}", project_b.join("include").display());
        let a = cache_key(&project_a, BuildProfile::Quick, &a_signature, &framework);
        let b = cache_key(&project_b, BuildProfile::Quick, &b_signature, &framework);
        assert_eq!(a, b);
    }

    #[test]
    fn cache_key_normalizes_windows_style_project_paths_in_flags() {
        let tmp = tempfile::tempdir().unwrap();
        let framework = tmp.path().join("framework");
        std::fs::create_dir_all(framework.join("WiFi/src")).unwrap();
        std::fs::write(framework.join("WiFi/src/WiFi.cpp"), "int wifi;").unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(project.join("include")).unwrap();

        let raw_signature = format!("-I{}", project.display());
        let slash_signature = format!("-I{}", project.to_string_lossy().replace('\\', "/"));
        let raw = cache_key(&project, BuildProfile::Quick, &raw_signature, &framework);
        let slash = cache_key(&project, BuildProfile::Quick, &slash_signature, &framework);
        assert_eq!(raw, slash);
    }

    #[test]
    fn header_changes_invalidate_the_cache_key() {
        let tmp = tempfile::tempdir().unwrap();
        let framework = tmp.path().join("framework");
        std::fs::create_dir_all(framework.join("WiFi/src")).unwrap();
        std::fs::write(framework.join("WiFi/src/WiFi.cpp"), "int wifi;").unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(project.join("include")).unwrap();
        let header = project.join("include/config.h");
        std::fs::write(&header, "#define WIFI 1").unwrap();
        let first = cache_key(&project, BuildProfile::Quick, "-O2", &framework);
        std::fs::write(&header, "#define WIFI 2").unwrap();
        let second = cache_key(&project, BuildProfile::Quick, "-O2", &framework);
        assert_ne!(first, second);
    }

    #[test]
    fn hydrate_restores_archives_and_preserves_failure_markers() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        let framework = tmp.path().join("framework");
        std::fs::create_dir_all(framework.join("WiFi/src")).unwrap();
        std::fs::write(framework.join("WiFi/src/WiFi.cpp"), "int wifi;").unwrap();
        let cache = FrameworkLibraryCache::with_cache_root(
            &project,
            &tmp.path().join("cache"),
            BuildProfile::Quick,
            "-O2",
            &framework,
        );
        let source_archive = tmp.path().join("libwifi.a");
        std::fs::write(&source_archive, "archive").unwrap();
        cache.store_archive(&source_archive).unwrap();
        cache.record_failure("matter").unwrap();

        let hydrated = tmp.path().join("hydrated");
        assert_eq!(cache.hydrate(&hydrated).unwrap(), 1);
        assert_eq!(std::fs::read(hydrated.join("libwifi.a")).unwrap(), b"archive");
        assert!(cache.has_failed("matter"));
        cache.remove().unwrap();
        assert!(!cache.has_failed("matter"));
    }
}
