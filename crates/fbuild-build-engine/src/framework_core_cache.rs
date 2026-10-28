//! Cross-run framework core artifact cache.
//!
//! Per-project `core/` build directories are intentionally local to a project,
//! but CI wants the expensive framework objects to survive across runs. This
//! module stores only the reusable core artifacts under
//! `~/.fbuild/{dev|prod}/cache/core/<hash>/` and hydrates project `core/` dirs
//! before normal incremental rebuild checks run.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use fbuild_core::BuildProfile;
use fbuild_core::path::NormalizedPath;
use sha2::{Digest, Sha256};

use crate::compiler::{Compiler, CompilerBase};
use crate::flag_overlay::LanguageExtraFlags;

const CORE_CACHE_VERSION: &str = "fbuild-core-artifacts-v1";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ArtifactCopyStats {
    pub copied: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone)]
pub struct FrameworkCoreCache {
    key: String,
    path: PathBuf,
}

impl FrameworkCoreCache {
    pub fn new(
        project_dir: &Path,
        platform_label: &str,
        env_name: &str,
        profile: BuildProfile,
        compiler: &dyn Compiler,
        core_sources: &[PathBuf],
        extra_flags: &LanguageExtraFlags,
    ) -> Self {
        let key = core_cache_key(
            project_dir,
            platform_label,
            env_name,
            profile,
            compiler,
            core_sources,
            extra_flags,
        );
        let path = fbuild_packages::Cache::new(project_dir)
            .core_artifacts_dir()
            .join(&key);
        Self { key, path }
    }

    /// Test seam: like [`FrameworkCoreCache::new`], but rooted at an explicit
    /// cache root instead of the process-global `~/.fbuild/{dev|prod}/cache`.
    /// Without this, unit tests write into (and read back from) the REAL user
    /// cache, so leftover artifacts from earlier runs change hydrate counts
    /// and make the tests both flaky and cache-polluting.
    /// Mirrors [`FrameworkCoreCache::new`]'s full signature plus the cache
    /// root, so the argument count is inherent to the seam.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    fn new_with_cache_root(
        cache_root: &Path,
        project_dir: &Path,
        platform_label: &str,
        env_name: &str,
        profile: BuildProfile,
        compiler: &dyn Compiler,
        core_sources: &[PathBuf],
        extra_flags: &LanguageExtraFlags,
    ) -> Self {
        let key = core_cache_key(
            project_dir,
            platform_label,
            env_name,
            profile,
            compiler,
            core_sources,
            extra_flags,
        );
        let path = fbuild_packages::Cache::with_cache_root(project_dir, cache_root)
            .core_artifacts_dir()
            .join(&key);
        Self { key, path }
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn hydrate(
        &self,
        core_build_dir: &Path,
        compiler: &dyn Compiler,
        core_sources: &[PathBuf],
        extra_flags: &LanguageExtraFlags,
    ) -> std::io::Result<ArtifactCopyStats> {
        if !self.path.is_dir() {
            return Ok(ArtifactCopyStats::default());
        }
        let mut outcome = copy_artifacts(&self.path, core_build_dir, false, false, true)?;
        if outcome.deliveries.aliased() > 0 {
            tracing::debug!(
                reflinked = outcome.deliveries.reflinked,
                hardlinked = outcome.deliveries.hardlinked,
                copied = outcome.deliveries.copied,
                "framework core cache hydrate delivered without byte copies"
            );
        }
        let refreshed = refresh_command_hashes(
            core_build_dir,
            compiler,
            core_sources,
            extra_flags,
            &outcome.copied_objects,
        )?;
        outcome.stats.copied += refreshed;
        Ok(outcome.stats)
    }

    pub fn store(&self, core_build_dir: &Path) -> std::io::Result<ArtifactCopyStats> {
        if !core_build_dir.is_dir() {
            return Ok(ArtifactCopyStats::default());
        }
        std::fs::create_dir_all(&self.path)?;
        // Never aliased: the cache owns its own bytes, so a later in-place
        // write in a project's build dir can never corrupt a shared entry.
        Ok(copy_artifacts(core_build_dir, &self.path, true, true, false)?.stats)
    }

    /// Build-output line for a [`Self::hydrate`] outcome. CI logs only carry
    /// build output, so this is how a restored cache shows it was used
    /// (FastLED/fbuild#1433).
    pub fn hydrate_summary(&self, outcome: &std::io::Result<ArtifactCopyStats>) -> String {
        let key = short_key(&self.key);
        match outcome {
            Ok(stats) if stats.copied > 0 || stats.skipped > 0 => format!(
                "framework core cache: hit key={key} copied={} skipped={}",
                stats.copied, stats.skipped
            ),
            Ok(_) => format!("framework core cache: miss key={key}"),
            Err(error) => format!("framework core cache: hydrate failed key={key}: {error}"),
        }
    }

    /// Build-output line for a [`Self::store`] outcome.
    pub fn store_summary(&self, outcome: &std::io::Result<ArtifactCopyStats>) -> String {
        let key = short_key(&self.key);
        match outcome {
            Ok(stats) if stats.copied > 0 => {
                format!(
                    "framework core cache: stored key={key} copied={}",
                    stats.copied
                )
            }
            Ok(_) => format!("framework core cache: up to date key={key}"),
            Err(error) => format!("framework core cache: store failed key={key}: {error}"),
        }
    }

    /// Remove only this content-addressed cache entry.
    ///
    /// The parent cache root may contain entries for other projects,
    /// environments, profiles, or compiler signatures and must remain intact.
    pub fn remove(&self) -> std::io::Result<()> {
        match std::fs::remove_dir_all(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

/// Enough of a sha256 key to tell entries apart in a log line.
fn short_key(key: &str) -> &str {
    &key[..key.len().min(12)]
}

fn core_cache_key(
    project_dir: &Path,
    platform_label: &str,
    env_name: &str,
    profile: BuildProfile,
    compiler: &dyn Compiler,
    core_sources: &[PathBuf],
    extra_flags: &LanguageExtraFlags,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(CORE_CACHE_VERSION.as_bytes());
    hasher.update([0]);
    hasher.update(env!("CARGO_PKG_VERSION").as_bytes());
    hasher.update([0]);
    hasher.update(platform_label.as_bytes());
    hasher.update([0]);
    hasher.update(env_name.as_bytes());
    hasher.update([0]);
    hasher.update(profile.as_dir_name().as_bytes());
    hasher.update([0]);

    let mut source_entries: Vec<_> = core_sources
        .iter()
        .map(|source| {
            // FastLED/fbuild#911 — path-shape slash normalization goes
            // through `NormalizedPath::display_slash()`.
            let source_path = if source.is_absolute() {
                fbuild_core::path::path_arg_for_compile_cwd(source, project_dir)
            } else {
                NormalizedPath::from(source.as_path()).display_slash()
            };
            let source_flags = extra_flags.for_source(source);
            let signature = compiler.artifact_cache_signature(project_dir, source, &source_flags);
            let content = file_content_digest(source);
            (source_path, signature, content)
        })
        .collect();
    source_entries.sort_by(|a, b| a.0.cmp(&b.0));

    for (source_path, signature, content) in source_entries {
        hasher.update(source_path.as_bytes());
        hasher.update([0]);
        hasher.update(signature.as_bytes());
        hasher.update([0]);
        hasher.update(content.as_bytes());
        hasher.update([0xff]);
    }

    format!("{:x}", hasher.finalize())
}

fn file_content_digest(path: &Path) -> String {
    match std::fs::read(path) {
        Ok(bytes) => {
            let mut hasher = Sha256::new();
            hasher.update(bytes);
            format!("sha256:{:x}", hasher.finalize())
        }
        Err(e) => format!("unreadable:{}", e.kind()),
    }
}

/// Copy one directory of artifacts into another.
///
/// `alias_ok` selects how each file is delivered: see [`deliver_artifact`].
/// Hydration may alias (the build dir only ever replaces cached objects, never
/// mutates them in place); [`FrameworkCoreCache::store`] may not, because the
/// cache has to own its bytes.
fn copy_artifacts(
    src_dir: &Path,
    dst_dir: &Path,
    overwrite: bool,
    include_cmdhash: bool,
    alias_ok: bool,
) -> std::io::Result<ArtifactCopyOutcome> {
    std::fs::create_dir_all(dst_dir)?;
    let mut outcome = ArtifactCopyOutcome::default();
    for entry in std::fs::read_dir(src_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let src = entry.path();
        if !is_core_artifact(&src, include_cmdhash) {
            continue;
        }
        let dst = dst_dir.join(entry.file_name());
        if dst.exists() {
            if !overwrite {
                outcome.stats.skipped += 1;
                continue;
            }
            std::fs::remove_file(&dst)?;
        }
        outcome
            .deliveries
            .record(deliver_artifact(&src, &dst, alias_ok)?);
        if is_object_artifact(&src) {
            outcome.copied_objects.insert(entry.file_name());
        }
        outcome.stats.copied += 1;
    }
    Ok(outcome)
}

#[derive(Default)]
struct ArtifactCopyOutcome {
    stats: ArtifactCopyStats,
    copied_objects: HashSet<std::ffi::OsString>,
    deliveries: DeliveryTally,
}

/// How each artifact reached the destination, for the hydrate debug line.
#[derive(Debug, Default, Clone, Copy)]
struct DeliveryTally {
    reflinked: usize,
    hardlinked: usize,
    copied: usize,
}

impl DeliveryTally {
    fn record(&mut self, delivery: Delivery) {
        match delivery {
            Delivery::Reflink => self.reflinked += 1,
            Delivery::HardLink => self.hardlinked += 1,
            Delivery::Copy => self.copied += 1,
        }
    }

    /// Deliveries that avoided copying the object's bytes.
    fn aliased(&self) -> usize {
        self.reflinked + self.hardlinked
    }
}

/// How a cached artifact reached its destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Delivery {
    /// Copy-on-write clone: an independent file sharing the original's
    /// extents (btrfs/XFS/APFS/ReFS). No bytes move and later writes to either
    /// file stay private.
    Reflink,
    /// A second hard link to the cached inode: free on every filesystem, but
    /// only ever into a build directory, and only on one volume.
    HardLink,
    /// A byte-for-byte copy: the portable fallback.
    Copy,
}

/// Deliver `src` to `dst`, preferring the cheapest tier the volume supports.
///
/// `hydrate` used to `fs::copy` all 118 framework core objects into every new
/// project directory: 1.17 s of pure byte movement per fresh build, on a
/// filesystem where nothing was reflinked (`reflink_count=0, copy_count=2` in
/// the zccache journal; FastLED/fbuild#1414 finding 3). Both cheaper tiers fall
/// back to the next one when the filesystem refuses them (`EOPNOTSUPP` on an
/// ext4 reflink, `EXDEV` for a hard link across volumes), so the caller never
/// has to probe the volume first and a copy is still the floor.
///
/// `alias_ok` is the caller's statement that the destination may share storage
/// with the source. It is false for `store`, where the cache must own its
/// bytes.
fn deliver_artifact(src: &Path, dst: &Path, alias_ok: bool) -> std::io::Result<Delivery> {
    if alias_ok {
        if reflink_copy::reflink(src, dst).is_ok() {
            copy_mtime(src, dst);
            return Ok(Delivery::Reflink);
        }
        // A hard link keeps the cached mtime for free and is supported
        // everywhere a build tree and the cache share a volume.
        if std::fs::hard_link(src, dst).is_ok() {
            return Ok(Delivery::HardLink);
        }
    }
    copy_preserving_mtime(src, dst)?;
    Ok(Delivery::Copy)
}

fn is_core_artifact(path: &Path, include_cmdhash: bool) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some("o" | "d")
    ) || (include_cmdhash
        && matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("cmdhash")
        ))
}

fn is_object_artifact(path: &Path) -> bool {
    matches!(path.extension().and_then(|ext| ext.to_str()), Some("o"))
}

fn refresh_command_hashes(
    core_build_dir: &Path,
    compiler: &dyn Compiler,
    core_sources: &[PathBuf],
    extra_flags: &LanguageExtraFlags,
    copied_objects: &HashSet<std::ffi::OsString>,
) -> std::io::Result<usize> {
    let mut refreshed = 0;
    for source in core_sources {
        let obj = CompilerBase::object_path(source, core_build_dir);
        let Some(name) = obj.file_name() else {
            continue;
        };
        if !copied_objects.contains(name) {
            continue;
        }
        let source_flags = extra_flags.for_source(source);
        let signature = compiler.rebuild_signature(source, &source_flags, &obj);
        std::fs::write(obj.with_extension("cmdhash"), signature)?;
        refreshed += 1;
    }
    Ok(refreshed)
}

fn copy_preserving_mtime(src: &Path, dst: &Path) -> std::io::Result<u64> {
    let bytes = std::fs::copy(src, dst)?;
    copy_mtime(src, dst);
    Ok(bytes)
}

/// Give `dst` the source's mtime. Incremental rebuild checks compare mtimes, so
/// a delivery that lost them would look like a stale object and recompile the
/// whole framework core — the exact cost the cache exists to avoid.
fn copy_mtime(src: &Path, dst: &Path) {
    if let (Ok(meta), Ok(file)) = (
        src.metadata(),
        std::fs::File::options().write(true).open(dst),
    ) {
        if let Ok(mtime) = meta.modified() {
            let _ = file.set_modified(mtime);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::{CompileResult, Compiler};

    struct FakeCompiler {
        gcc: PathBuf,
        gxx: PathBuf,
    }

    impl FakeCompiler {
        fn new() -> Self {
            Self {
                gcc: PathBuf::from("/toolchain/bin/gcc"),
                gxx: PathBuf::from("/toolchain/bin/g++"),
            }
        }
    }

    #[async_trait::async_trait]
    impl Compiler for FakeCompiler {
        async fn compile_one(
            &self,
            _compiler_path: &Path,
            _source: &Path,
            output: &Path,
            _flags: &[String],
            _extra_flags: &[String],
        ) -> fbuild_core::Result<CompileResult> {
            Ok(CompileResult {
                success: true,
                object_file: output.to_path_buf(),
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 0,
            })
        }

        fn gcc_path(&self) -> &Path {
            &self.gcc
        }

        fn gxx_path(&self) -> &Path {
            &self.gxx
        }

        fn c_flags(&self) -> Vec<String> {
            vec!["-Os".to_string()]
        }

        fn cpp_flags(&self) -> Vec<String> {
            vec!["-Os".to_string(), "-std=gnu++17".to_string()]
        }

        fn rebuild_signature(
            &self,
            source: &Path,
            extra_flags: &[String],
            _output: &Path,
        ) -> String {
            let mut hasher = Sha256::new();
            hasher.update(source.to_string_lossy().as_bytes());
            for flag in extra_flags {
                hasher.update([0]);
                hasher.update(flag.as_bytes());
            }
            format!("{:x}", hasher.finalize())
        }
    }

    fn cache_with_key(key: &str) -> FrameworkCoreCache {
        FrameworkCoreCache {
            key: key.to_string(),
            path: PathBuf::from("/cache/core").join(key),
        }
    }

    #[test]
    fn hydrate_summary_names_hit_miss_and_failure() {
        let cache = cache_with_key("0123456789abcdef0123");
        let hit = cache.hydrate_summary(&Ok(ArtifactCopyStats {
            copied: 3,
            skipped: 1,
        }));
        assert_eq!(
            hit,
            "framework core cache: hit key=0123456789ab copied=3 skipped=1"
        );
        let miss = cache.hydrate_summary(&Ok(ArtifactCopyStats::default()));
        assert_eq!(miss, "framework core cache: miss key=0123456789ab");
        let failed = cache.hydrate_summary(&Err(std::io::Error::other("disk gone")));
        assert_eq!(
            failed,
            "framework core cache: hydrate failed key=0123456789ab: disk gone"
        );
    }

    #[test]
    fn store_summary_names_stored_up_to_date_and_failure() {
        let cache = cache_with_key("fedcba9876543210");
        let stored = cache.store_summary(&Ok(ArtifactCopyStats {
            copied: 5,
            skipped: 0,
        }));
        assert_eq!(
            stored,
            "framework core cache: stored key=fedcba987654 copied=5"
        );
        let unchanged = cache.store_summary(&Ok(ArtifactCopyStats::default()));
        assert_eq!(
            unchanged,
            "framework core cache: up to date key=fedcba987654"
        );
        let failed = cache.store_summary(&Err(std::io::Error::other("read-only")));
        assert_eq!(
            failed,
            "framework core cache: store failed key=fedcba987654: read-only"
        );
    }

    #[test]
    fn key_changes_when_source_flags_change() {
        let compiler = FakeCompiler::new();
        let sources = vec![PathBuf::from("/framework/core/main.cpp")];
        let plain = LanguageExtraFlags {
            common: Vec::new(),
            c: Vec::new(),
            cxx: Vec::new(),
            asm: Vec::new(),
        };
        let flagged = LanguageExtraFlags {
            common: vec!["-DARDUINO=10819".to_string()],
            c: Vec::new(),
            cxx: Vec::new(),
            asm: Vec::new(),
        };
        let a = core_cache_key(
            Path::new("/project"),
            "avr",
            "uno",
            BuildProfile::Release,
            &compiler,
            &sources,
            &plain,
        );
        let b = core_cache_key(
            Path::new("/project"),
            "avr",
            "uno",
            BuildProfile::Release,
            &compiler,
            &sources,
            &flagged,
        );
        assert_ne!(a, b);
    }

    #[test]
    fn key_is_independent_of_source_order() {
        let compiler = FakeCompiler::new();
        let tmp = tempfile::tempdir().unwrap();
        let a_source = tmp.path().join("a.cpp");
        let b_source = tmp.path().join("b.cpp");
        std::fs::write(&a_source, b"a").unwrap();
        std::fs::write(&b_source, b"b").unwrap();
        let plain = LanguageExtraFlags {
            common: Vec::new(),
            c: Vec::new(),
            cxx: Vec::new(),
            asm: Vec::new(),
        };

        let a = core_cache_key(
            tmp.path(),
            "avr",
            "uno",
            BuildProfile::Release,
            &compiler,
            &[a_source.clone(), b_source.clone()],
            &plain,
        );
        let b = core_cache_key(
            tmp.path(),
            "avr",
            "uno",
            BuildProfile::Release,
            &compiler,
            &[b_source, a_source],
            &plain,
        );
        assert_eq!(a, b);
    }

    #[test]
    fn key_changes_when_source_content_changes() {
        let compiler = FakeCompiler::new();
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("main.cpp");
        let plain = LanguageExtraFlags {
            common: Vec::new(),
            c: Vec::new(),
            cxx: Vec::new(),
            asm: Vec::new(),
        };

        std::fs::write(&source, b"first").unwrap();
        let first = core_cache_key(
            tmp.path(),
            "avr",
            "uno",
            BuildProfile::Release,
            &compiler,
            std::slice::from_ref(&source),
            &plain,
        );
        std::fs::write(&source, b"second").unwrap();
        let second = core_cache_key(
            tmp.path(),
            "avr",
            "uno",
            BuildProfile::Release,
            &compiler,
            std::slice::from_ref(&source),
            &plain,
        );
        assert_ne!(first, second);
    }

    #[test]
    fn key_is_independent_of_project_directory_name() {
        let compiler = FakeCompiler::new();
        let tmp = tempfile::tempdir().unwrap();
        let project_a = tmp.path().join("nds");
        let project_b = tmp.path().join("nds-copy");
        let source_a = project_a.join("src").join("main.cpp");
        let source_b = project_b.join("src").join("main.cpp");
        std::fs::create_dir_all(source_a.parent().unwrap()).unwrap();
        std::fs::create_dir_all(source_b.parent().unwrap()).unwrap();
        std::fs::write(&source_a, b"same core").unwrap();
        std::fs::write(&source_b, b"same core").unwrap();
        let plain = LanguageExtraFlags {
            common: Vec::new(),
            c: Vec::new(),
            cxx: Vec::new(),
            asm: Vec::new(),
        };

        let a = core_cache_key(
            &project_a,
            "esp32",
            "esp32dev",
            BuildProfile::Release,
            &compiler,
            &[source_a],
            &plain,
        );
        let b = core_cache_key(
            &project_b,
            "esp32",
            "esp32dev",
            BuildProfile::Release,
            &compiler,
            &[source_b],
            &plain,
        );

        assert_eq!(a, b);
    }

    #[test]
    fn hydrate_and_store_only_core_artifact_files() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        // Hermetic cache root: rooting at the real user cache made this test
        // accumulate one obj+dep pair per run (the object filename hashes the
        // per-run tempdir source path while the cache key stays constant), so
        // hydrate's copied count grew 3, 5, 7, ... across repeated runs.
        let cache_root = tmp.path().join("cache-root");
        let cache = FrameworkCoreCache::new_with_cache_root(
            &cache_root,
            &project,
            "avr",
            "uno",
            BuildProfile::Release,
            &FakeCompiler::new(),
            &[PathBuf::from("/framework/core/main.cpp")],
            &LanguageExtraFlags {
                common: Vec::new(),
                c: Vec::new(),
                cxx: Vec::new(),
                asm: Vec::new(),
            },
        );

        let core = tmp.path().join("core");
        std::fs::create_dir_all(&core).unwrap();
        let source = project.join("src").join("main.cpp");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, b"int main() { return 0; }\n").unwrap();
        let object = CompilerBase::object_path(&source, &core);
        let depfile = object.with_extension("d");
        let cmdhash = object.with_extension("cmdhash");
        std::fs::write(&object, b"obj").unwrap();
        std::fs::write(&depfile, b"dep").unwrap();
        std::fs::write(&cmdhash, b"hash").unwrap();
        std::fs::write(core.join("note.txt"), b"ignore").unwrap();

        let stored = cache.store(&core).unwrap();
        assert_eq!(stored.copied, 3);
        assert!(cache.path().join(object.file_name().unwrap()).exists());
        assert!(!cache.path().join("note.txt").exists());

        let hydrated = tmp.path().join("hydrated");
        let flags = LanguageExtraFlags {
            common: Vec::new(),
            c: Vec::new(),
            cxx: Vec::new(),
            asm: Vec::new(),
        };
        let compiler = FakeCompiler::new();
        let sources = vec![source.clone()];
        let stats = cache
            .hydrate(&hydrated, &compiler, &sources, &flags)
            .unwrap();
        assert_eq!(stats.copied, 3);
        let hydrated_object = hydrated.join(object.file_name().unwrap());
        let hydrated_cmdhash = hydrated_object.with_extension("cmdhash");
        assert_eq!(std::fs::read(&hydrated_object).unwrap(), b"obj");
        assert_eq!(
            std::fs::read_to_string(hydrated_cmdhash).unwrap(),
            compiler.rebuild_signature(&source, &[], &hydrated_object)
        );
    }

    #[test]
    fn hydrate_skips_existing_project_artifacts() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(src.join("main.cpp.o"), b"cache").unwrap();
        std::fs::write(dst.join("main.cpp.o"), b"project").unwrap();

        let outcome = copy_artifacts(&src, &dst, false, true, false).unwrap();
        assert_eq!(outcome.stats.copied, 0);
        assert_eq!(outcome.stats.skipped, 1);
        assert_eq!(std::fs::read(dst.join("main.cpp.o")).unwrap(), b"project");
    }

    #[test]
    fn remove_deletes_only_the_selected_cache_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let compiler = FakeCompiler::new();
        let flags = LanguageExtraFlags {
            common: Vec::new(),
            c: Vec::new(),
            cxx: Vec::new(),
            asm: Vec::new(),
        };
        let source = project.join("src/main.cpp");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, b"same").unwrap();
        // Hermetic cache root (see hydrate_and_store_only_core_artifact_files):
        // never create/remove entries under the real user cache from a test.
        let cache_root = tmp.path().join("cache-root");
        let selected = FrameworkCoreCache::new_with_cache_root(
            &cache_root,
            &project,
            "avr",
            "uno",
            BuildProfile::Release,
            &compiler,
            std::slice::from_ref(&source),
            &flags,
        );
        let sibling = FrameworkCoreCache::new_with_cache_root(
            &cache_root,
            &project,
            "avr",
            "mega",
            BuildProfile::Release,
            &compiler,
            std::slice::from_ref(&source),
            &flags,
        );
        std::fs::create_dir_all(selected.path()).unwrap();
        std::fs::create_dir_all(sibling.path()).unwrap();
        std::fs::write(selected.path().join("marker"), b"selected").unwrap();
        std::fs::write(sibling.path().join("marker"), b"sibling").unwrap();

        selected.remove().unwrap();

        assert!(!selected.path().exists());
        assert_eq!(
            std::fs::read(sibling.path().join("marker")).unwrap(),
            b"sibling"
        );
        selected.remove().unwrap();
    }

    /// An mtime old enough that "preserved" cannot be confused with "just
    /// written", which is what makes the assertions below meaningful.
    fn ancient_mtime() -> std::time::SystemTime {
        std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000)
    }

    fn set_mtime(path: &Path, mtime: std::time::SystemTime) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
    }

    fn read_mtime(path: &Path) -> std::time::SystemTime {
        std::fs::metadata(path).unwrap().modified().unwrap()
    }

    #[test]
    fn alias_delivery_avoids_copying_bytes_and_keeps_the_mtime() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("cached.o");
        let dst = tmp.path().join("hydrated.o");
        std::fs::write(&src, b"object bytes").unwrap();
        set_mtime(&src, ancient_mtime());

        let delivery = deliver_artifact(&src, &dst, true).unwrap();

        // Every normal filesystem supports a hard link, so aliasing must never
        // degrade to a copy when the caller allows it.
        assert_ne!(delivery, Delivery::Copy);
        assert_eq!(std::fs::read(&dst).unwrap(), b"object bytes");
        // A lost mtime makes an incremental check recompile the whole core.
        assert_eq!(read_mtime(&dst), ancient_mtime());
    }

    #[test]
    fn delivery_without_aliasing_copies_and_stays_independent() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("cached.o");
        let dst = tmp.path().join("stored.o");
        std::fs::write(&src, b"object bytes").unwrap();
        set_mtime(&src, ancient_mtime());

        let delivery = deliver_artifact(&src, &dst, false).unwrap();

        assert_eq!(delivery, Delivery::Copy);
        assert_eq!(read_mtime(&dst), ancient_mtime());
        // An in-place write to one file must not be visible in the other: that
        // aliasing is exactly what `store` may not create.
        std::fs::write(&dst, b"changed!!!!!").unwrap();
        assert_eq!(std::fs::read(&src).unwrap(), b"object bytes");
    }

    #[test]
    fn store_owns_its_bytes_even_though_hydrate_aliases() {
        let tmp = tempfile::tempdir().unwrap();
        let build = tmp.path().join("build");
        let cache = tmp.path().join("cache");
        std::fs::create_dir_all(&build).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(build.join("main.cpp.o"), b"object bytes").unwrap();

        copy_artifacts(&build, &cache, true, true, false).unwrap();

        // In-place mutation of the cached entry must leave the build dir alone.
        std::fs::write(cache.join("main.cpp.o"), b"corrupted!!!").unwrap();
        assert_eq!(
            std::fs::read(build.join("main.cpp.o")).unwrap(),
            b"object bytes"
        );
    }

    #[test]
    fn hydrate_artifacts_alias_when_the_caller_allows_it() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("cache-entry");
        let dst = tmp.path().join("core");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a.cpp.o"), b"a").unwrap();
        std::fs::write(src.join("b.cpp.o"), b"b").unwrap();
        std::fs::write(src.join("ignored.txt"), b"no").unwrap();

        let aliased = copy_artifacts(&src, &dst, false, false, true).unwrap();
        assert_eq!(aliased.stats.copied, 2);
        assert_eq!(aliased.deliveries.aliased(), 2);
        assert_eq!(aliased.deliveries.copied, 0);

        let fresh = tmp.path().join("fresh");
        let copied = copy_artifacts(&src, &fresh, false, false, false).unwrap();
        assert_eq!(copied.stats.copied, 2);
        assert_eq!(copied.deliveries.aliased(), 0);
        assert_eq!(copied.deliveries.copied, 2);
    }
}
