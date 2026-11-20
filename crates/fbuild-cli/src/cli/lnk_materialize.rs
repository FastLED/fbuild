//! Where `fbuild lnk pull --materialize` writes resolved blobs.
//!
//! A pointer's job is to name a *source*, not a destination: git-LFS, DVC,
//! Bazel and Nix all keep placement out of the descriptor, and the
//! `fbuild lnk` audit on FastLED/fbuild#1356 concluded the same for `.fetch`
//! (Q1 — "do not add `dest`"). So the destination comes from the command
//! line, and there are exactly three:
//!
//! * the build tree, `<build root>/resources/<rel>` — the same projection the
//!   build itself uses, and the default,
//! * `--materialize-dir <DIR>` — a release script or a VFS packer that wants
//!   the blobs somewhere else,
//! * `--in-place` — beside the pointer (`data/x.bin.fetch` → `data/x.bin`),
//!   which is where an SD-card copy expects to find the file.
//!
//! The first two keep the source tree clean (the reason materialization goes
//! to a build tree at all); `--in-place` deliberately does not, so it also
//! records each generated name in the scanned root's `.gitignore`.

use std::path::{Path, PathBuf};

use fbuild_core::path::NormalizedPath;
use fbuild_core::{FbuildError, Result};
use fbuild_packages::DiskCache;
use fbuild_packages::lnk::{
    DiscoveredLnk, MaterializedLnk, materialize_one, strip_pointer_extension,
};

/// The destination `lnk pull --materialize` selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MaterializeMode {
    /// `<build root>/resources` — the build tree's own projection.
    BuildResources,
    /// An explicit directory.
    Dir(NormalizedPath),
    /// Beside each pointer, pointer suffix stripped.
    InPlace,
}

/// What a materialization pass did.
#[derive(Debug, Default)]
pub(crate) struct MaterializeOutcome {
    /// One entry per pointer that was written out.
    pub written: Vec<MaterializedLnk>,
    /// `(pointer, reason)` for every pointer that could not be written. One
    /// bad pointer does not stop the rest: the cached blobs that do resolve
    /// are still usable.
    pub failures: Vec<(PathBuf, String)>,
}

impl MaterializeMode {
    /// Read the three flags into a mode, or `None` when the user asked for a
    /// cache-only pull (the historical behaviour).
    ///
    /// `--materialize-dir` and `--in-place` imply `--materialize`, so a user
    /// who only wants files does not have to pass two flags.
    pub(crate) fn from_flags(
        materialize: bool,
        materialize_dir: Option<&str>,
        in_place: bool,
    ) -> Result<Option<Self>> {
        if in_place {
            if materialize_dir.is_some() {
                return Err(FbuildError::ConfigError(
                    "--in-place writes beside each pointer, so it cannot be combined with \
                     --materialize-dir"
                        .to_string(),
                ));
            }
            return Ok(Some(Self::InPlace));
        }
        if let Some(dir) = materialize_dir {
            let dir = dir.trim();
            if dir.is_empty() {
                return Err(FbuildError::ConfigError(
                    "--materialize-dir needs a directory".to_string(),
                ));
            }
            return Ok(Some(Self::Dir(NormalizedPath::new(PathBuf::from(dir)))));
        }
        Ok(materialize.then_some(Self::BuildResources))
    }

    /// Where the blob behind `lnk_path` lands.
    ///
    /// `scan_root` is the tree the pointers were discovered under; the
    /// pointer's path relative to it is what keeps a project's directory
    /// shape in the destination, so `data/video1.rgb.fetch` becomes
    /// `<dest>/data/video1.rgb` rather than a flat pile of blobs.
    pub(crate) fn target(&self, scan_root: &Path, lnk_path: &Path) -> Result<PathBuf> {
        let file_name = lnk_path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| {
                FbuildError::ConfigError(format!(
                    "cannot decode blob-pointer file name: {}",
                    lnk_path.display()
                ))
            })?;
        let projected = strip_pointer_extension(file_name).ok_or_else(|| {
            FbuildError::ConfigError(format!(
                "{} is not a blob pointer fbuild can materialize",
                lnk_path.display()
            ))
        })?;

        match self {
            Self::InPlace => {
                let parent = lnk_path.parent().unwrap_or_else(|| Path::new(""));
                Ok(parent.join(projected))
            }
            Self::BuildResources | Self::Dir(_) => {
                let relative = lnk_path.strip_prefix(scan_root).map_err(|_| {
                    FbuildError::ConfigError(format!(
                        "{} is not under the scanned root {}",
                        lnk_path.display(),
                        scan_root.display()
                    ))
                })?;
                let relative = relative
                    .parent()
                    .map(|dir| dir.join(projected))
                    .unwrap_or_else(|| PathBuf::from(projected));
                Ok(self.root(scan_root).join(relative))
            }
        }
    }

    /// The directory every target is rooted in.
    pub(crate) fn root(&self, scan_root: &Path) -> PathBuf {
        match self {
            Self::BuildResources => {
                fbuild_paths::get_project_build_root(scan_root).join("resources")
            }
            Self::Dir(dir) => dir.as_path().to_path_buf(),
            Self::InPlace => scan_root.to_path_buf(),
        }
    }

    /// One-line description for the run summary.
    pub(crate) fn describe(&self, scan_root: &Path) -> String {
        match self {
            Self::BuildResources => format!("build tree ({})", self.root(scan_root).display()),
            Self::Dir(dir) => dir.as_path().display().to_string(),
            Self::InPlace => "beside each pointer".to_string(),
        }
    }
}

/// Materialize every discovered pointer under `mode`.
///
/// The returned [`MaterializedLnk`] values hold the cache lease that pins
/// their blob against GC, so the caller keeps the outcome alive for as long as
/// it needs the files on disk.
pub(crate) fn materialize_discovered(
    discovered: &[DiscoveredLnk],
    scan_root: &Path,
    mode: &MaterializeMode,
    cache: &DiskCache,
) -> MaterializeOutcome {
    let mut outcome = MaterializeOutcome::default();
    for entry in discovered {
        let target = match mode.target(scan_root, &entry.path) {
            Ok(target) => target,
            Err(e) => {
                outcome.failures.push((entry.path.clone(), e.to_string()));
                continue;
            }
        };
        match materialize_one(&entry.path, &entry.lnk, &target, cache) {
            Ok(materialized) => outcome.written.push(materialized),
            Err(e) => outcome.failures.push((entry.path.clone(), e.to_string())),
        }
    }
    outcome
}

/// `--in-place` puts generated files in the source tree, so the names are
/// recorded in the scanned root's `.gitignore` — the audit's mitigation for
/// the stale-file / accidental-commit risk that in-place mode reintroduces.
///
/// Idempotent: a name already listed (however it was spelled) is not added
/// twice. Returns the names that were newly added.
pub(crate) fn record_gitignore(
    scan_root: &Path,
    written: &[MaterializedLnk],
) -> Result<Vec<String>> {
    let mut wanted: Vec<String> = Vec::new();
    for entry in written {
        let Ok(relative) = entry.target_path.strip_prefix(scan_root) else {
            continue;
        };
        let name = relative.to_string_lossy().replace('\\', "/");
        if !name.is_empty() && !wanted.contains(&name) {
            wanted.push(name);
        }
    }
    if wanted.is_empty() {
        return Ok(wanted);
    }

    let path = scan_root.join(".gitignore");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let listed: Vec<&str> = existing.lines().map(str::trim).collect();
    let missing: Vec<&String> = wanted
        .iter()
        .filter(|name| !listed.iter().any(|line| line == name))
        .collect();
    if missing.is_empty() {
        return Ok(Vec::new());
    }

    let mut text = existing.clone();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if existing.is_empty() {
        text.push_str("# fbuild lnk pull --materialize --in-place\n");
    }
    for name in &missing {
        text.push_str(name);
        text.push('\n');
    }
    std::fs::write(&path, text).map_err(|e| {
        FbuildError::BuildFailed(format!("failed to update {}: {e}", path.display()))
    })?;
    Ok(missing.into_iter().cloned().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir_in(fbuild_paths::dev_or_prod_temp_root()).expect("scratch tempdir")
    }

    fn sha256_of(bytes: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(bytes);
        format!("{:x}", h.finalize())
    }

    /// Pre-stage a blob in the cache so `resolve` takes the cache-hit path and
    /// no test touches the network.
    fn stage_in_cache(cache: &DiskCache, url: &str, sha: &str, bytes: &[u8]) {
        let dir = cache.archive_dir(fbuild_packages::disk_cache::Kind::LnkBlobs, url, sha);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("blob");
        std::fs::write(&path, bytes).unwrap();
        cache
            .record_archive(
                fbuild_packages::disk_cache::Kind::LnkBlobs,
                url,
                sha,
                &path.to_string_lossy(),
                bytes.len() as i64,
                sha,
            )
            .unwrap();
    }

    fn write_pointer(root: &Path, relative: &str, url: &str, sha: &str, size: usize) -> PathBuf {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            format!(r#"{{"v":1,"url":"{url}","sha256":"{sha}","size":{size}}}"#),
        )
        .unwrap();
        path
    }

    #[test]
    fn no_materialize_flags_means_cache_only() {
        assert_eq!(
            MaterializeMode::from_flags(false, None, false).unwrap(),
            None
        );
        assert_eq!(
            MaterializeMode::from_flags(true, None, false).unwrap(),
            Some(MaterializeMode::BuildResources)
        );
    }

    #[test]
    fn materialize_dir_and_in_place_imply_materialize() {
        assert!(matches!(
            MaterializeMode::from_flags(false, Some("out"), false).unwrap(),
            Some(MaterializeMode::Dir(_))
        ));
        assert_eq!(
            MaterializeMode::from_flags(false, None, true).unwrap(),
            Some(MaterializeMode::InPlace)
        );
    }

    #[test]
    fn in_place_with_materialize_dir_is_refused() {
        let err = MaterializeMode::from_flags(false, Some("out"), true).unwrap_err();
        assert!(
            err.to_string().contains("--in-place"),
            "error should name the conflicting flag: {err}"
        );
    }

    #[test]
    fn default_root_is_the_build_tree_resources_dir() {
        let work = tempdir();
        let root = MaterializeMode::BuildResources.root(work.path());
        assert!(
            root.ends_with("resources"),
            "default destination must be a resources dir: {}",
            root.display()
        );
    }

    /// The pointer's own directory shape is preserved: a blob pointed at from
    /// `data/` lands in `data/` relative to the destination, never flat.
    #[test]
    fn target_preserves_the_relative_path() {
        let work = tempdir();
        let scan_root = work.path();
        let pointer = scan_root.join("assets/sfx/boom.wav.fetch");
        let mode = MaterializeMode::Dir(NormalizedPath::new(work.path().join("out")));
        assert_eq!(
            mode.target(scan_root, &pointer).unwrap(),
            work.path().join("out/assets/sfx/boom.wav")
        );
    }

    #[test]
    fn in_place_target_sits_beside_the_pointer() {
        let work = tempdir();
        let pointer = work.path().join("data/video1.rgb.fetch");
        assert_eq!(
            MaterializeMode::InPlace
                .target(work.path(), &pointer)
                .unwrap(),
            work.path().join("data/video1.rgb")
        );
    }

    /// End to end: a cached blob is written out at the projected name, and the
    /// pointer file itself is never copied into the destination.
    #[test]
    fn materialized_blob_matches_its_sha256_and_skips_the_pointer() {
        let work = tempdir();
        let scan_root = work.path().join("project");
        let bytes = b"littlefs payload".to_vec();
        let sha = sha256_of(&bytes);
        let url = "https://example.invalid/payload.bin";
        let pointer = write_pointer(&scan_root, "data/payload.bin.fetch", url, &sha, bytes.len());

        let cache = DiskCache::open_at(&work.path().join("cache")).unwrap();
        stage_in_cache(&cache, url, &sha, &bytes);

        let discovered = fbuild_packages::lnk::scan_for_lnk(&scan_root).unwrap();
        assert_eq!(discovered.len(), 1, "scanner should find the one pointer");

        let mode = MaterializeMode::Dir(NormalizedPath::new(work.path().join("out")));
        let outcome = materialize_discovered(&discovered, &scan_root, &mode, &cache);
        assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
        assert_eq!(outcome.written.len(), 1);

        let landed = work.path().join("out/data/payload.bin");
        assert_eq!(
            std::fs::read(&landed).unwrap(),
            bytes,
            "materialized file must be byte-identical to the cached blob"
        );
        assert!(
            !work.path().join("out/data/payload.bin.fetch").exists(),
            "the pointer must not land in the destination"
        );
        assert!(
            pointer.exists(),
            "materializing into a directory must leave the source tree alone"
        );
        assert_eq!(outcome.written[0].target_path, landed);
    }

    /// One pointer that cannot be written must not hide the ones that can.
    ///
    /// The failing case is a target path already occupied by a directory, so
    /// the failure is deterministic and offline — a *missing* blob would send
    /// the resolver into its download retry budget, which is minutes of wall
    /// clock the unit suite must not spend (FastLED/fbuild#806).
    #[test]
    fn a_failing_pointer_does_not_stop_the_other_pointers() {
        let work = tempdir();
        let scan_root = work.path().join("project");
        let good = b"present".to_vec();
        let good_sha = sha256_of(&good);
        let good_url = "https://example.invalid/present.bin";
        write_pointer(
            &scan_root,
            "data/present.bin.fetch",
            good_url,
            &good_sha,
            good.len(),
        );
        let blocked = b"blocked".to_vec();
        let blocked_sha = sha256_of(&blocked);
        let blocked_url = "https://example.invalid/blocked.bin";
        write_pointer(
            &scan_root,
            "data/blocked.bin.fetch",
            blocked_url,
            &blocked_sha,
            blocked.len(),
        );

        let cache = DiskCache::open_at(&work.path().join("cache")).unwrap();
        stage_in_cache(&cache, good_url, &good_sha, &good);
        stage_in_cache(&cache, blocked_url, &blocked_sha, &blocked);

        // A directory where the second blob wants to land.
        std::fs::create_dir_all(work.path().join("out/data/blocked.bin")).unwrap();

        let discovered = fbuild_packages::lnk::scan_for_lnk(&scan_root).unwrap();
        let mode = MaterializeMode::Dir(NormalizedPath::new(work.path().join("out")));
        let outcome = materialize_discovered(&discovered, &scan_root, &mode, &cache);

        assert_eq!(outcome.written.len(), 1, "the writable blob still lands");
        assert_eq!(outcome.failures.len(), 1, "the blocked pointer is reported");
        assert!(
            outcome.failures[0].0.ends_with("blocked.bin.fetch"),
            "the failure names the pointer: {:?}",
            outcome.failures[0].0
        );
        assert_eq!(
            std::fs::read(work.path().join("out/data/present.bin")).unwrap(),
            good
        );
    }

    /// In-place mode is the documented escape hatch: the file lands beside the
    /// pointer, the pointer stays put, and the name is gitignored so an
    /// accidental `git add .` cannot commit generated bytes.
    #[test]
    fn in_place_writes_beside_the_pointer_and_gitignores_it() {
        let work = tempdir();
        let scan_root = work.path().to_path_buf();
        let bytes = b"sd card asset".to_vec();
        let sha = sha256_of(&bytes);
        let url = "https://example.invalid/sd.bin";
        let pointer = write_pointer(&scan_root, "data/sd.bin.fetch", url, &sha, bytes.len());

        let cache = DiskCache::open_at(&work.path().join("cache")).unwrap();
        stage_in_cache(&cache, url, &sha, &bytes);

        let discovered = fbuild_packages::lnk::scan_for_lnk(&scan_root).unwrap();
        let outcome =
            materialize_discovered(&discovered, &scan_root, &MaterializeMode::InPlace, &cache);
        assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);

        let landed = scan_root.join("data/sd.bin");
        assert_eq!(std::fs::read(&landed).unwrap(), bytes);
        assert!(pointer.exists(), "in-place must not delete the pointer");
        assert_eq!(outcome.written[0].target_path, landed);

        let added = record_gitignore(&scan_root, &outcome.written).unwrap();
        assert_eq!(added, vec!["data/sd.bin".to_string()]);
        let gitignore = std::fs::read_to_string(scan_root.join(".gitignore")).unwrap();
        assert!(gitignore.contains("data/sd.bin"));

        // Second pass must not duplicate the entry.
        let added_again = record_gitignore(&scan_root, &outcome.written).unwrap();
        assert!(added_again.is_empty(), "entries must not be duplicated");
        assert_eq!(
            std::fs::read_to_string(scan_root.join(".gitignore"))
                .unwrap()
                .matches("data/sd.bin")
                .count(),
            1
        );
    }
}
