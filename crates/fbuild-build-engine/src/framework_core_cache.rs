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
        let mut outcome = copy_artifacts(&self.path, core_build_dir, false, false)?;
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
        Ok(copy_artifacts(core_build_dir, &self.path, true, true)?.stats)
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

fn copy_artifacts(
    src_dir: &Path,
    dst_dir: &Path,
    overwrite: bool,
    include_cmdhash: bool,
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
        copy_preserving_mtime(&src, &dst)?;
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
        let signature = compiler.rebuild_signature(source, &source_flags);
        std::fs::write(obj.with_extension("cmdhash"), signature)?;
        refreshed += 1;
    }
    Ok(refreshed)
}

fn copy_preserving_mtime(src: &Path, dst: &Path) -> std::io::Result<u64> {
    let bytes = std::fs::copy(src, dst)?;
    if let (Ok(meta), Ok(file)) = (
        src.metadata(),
        std::fs::File::options().write(true).open(dst),
    ) {
        if let Ok(mtime) = meta.modified() {
            let _ = file.set_modified(mtime);
        }
    }
    Ok(bytes)
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

        fn rebuild_signature(&self, source: &Path, extra_flags: &[String]) -> String {
            let mut hasher = Sha256::new();
            hasher.update(source.to_string_lossy().as_bytes());
            for flag in extra_flags {
                hasher.update([0]);
                hasher.update(flag.as_bytes());
            }
            format!("{:x}", hasher.finalize())
        }
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
        assert_eq!(std::fs::read(hydrated_object).unwrap(), b"obj");
        assert_eq!(
            std::fs::read_to_string(hydrated_cmdhash).unwrap(),
            compiler.rebuild_signature(&source, &[])
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

        let outcome = copy_artifacts(&src, &dst, false, true).unwrap();
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
}
