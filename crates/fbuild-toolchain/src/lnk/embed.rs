//! Glue between PlatformIO `embed_files`-shaped string lists and the
//! `.lnk` materializer.
//!
//! `expand_lnk_entries` takes a list of relative file paths (as produced
//! by `IniConfig::get_embed_files`/`get_embed_txtfiles`), runs each through
//! a caller-supplied `.lnk` resolver, and returns the absolute on-disk
//! paths that the build pipeline should actually feed to `objcopy`.
//!
//! Why a closure instead of a hard dependency on `DiskCache`:
//!
//! - keeps the helper testable without spinning up a real cache (tests
//!   can pass `|p| Ok(stub_path)`)
//! - lets pipelines that want offline-only behavior plug in a resolver
//!   that errors on cache miss
//! - lets future pipelines swap in alternate fetchers (s3, gh release,
//!   git LFS) without changing this seam

use std::path::{Path, PathBuf};

use fbuild_core::{FbuildError, Result};

use super::format::LnkFile;
use super::materialize::{MaterializedLnk, materialize_one};
use crate::DiskCache;

/// Resolve every entry to an absolute on-disk path. Entries ending in
/// `.lnk` are passed to `resolver`, which is expected to materialize the
/// blob and return the path of the resulting file. Other entries are
/// resolved relative to `project_dir` (or kept as-is if already absolute).
///
/// Errors short-circuit — one bad `.lnk` fails the whole expansion. This
/// is intentional: a missing resource is a build error, not a warning.
pub fn expand_lnk_entries<R>(
    entries: &[String],
    project_dir: &Path,
    mut resolver: R,
) -> Result<Vec<PathBuf>>
where
    R: FnMut(&Path) -> Result<PathBuf>,
{
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let entry_path = make_absolute(entry, project_dir);
        if has_lnk_extension(&entry_path) {
            let resolved = resolver(&entry_path)?;
            out.push(resolved);
        } else {
            out.push(entry_path);
        }
    }
    Ok(out)
}

/// Whether the given path's filename ends in `.lnk` (case-sensitive,
/// matching the convention of the rest of the module).
pub fn has_lnk_extension(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("lnk")
}

fn make_absolute(entry: &str, project_dir: &Path) -> PathBuf {
    let p = Path::new(entry);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        project_dir.join(p)
    }
}

/// One-shot helper for the common case: parse a `.lnk` from disk and
/// materialize it under `materialized_root/<basename-without-.lnk>`. Used
/// by build-system orchestrators that just want "give me the resolved
/// path" without writing the resolver closure boilerplate themselves.
///
/// The caller is responsible for keeping the returned `MaterializedLnk`
/// alive for the duration of the build — dropping it releases the cache
/// lease, which lets GC reap the blob.
pub fn materialize_lnk_entry(
    lnk_path: &Path,
    materialized_root: &Path,
    cache: &DiskCache,
) -> Result<MaterializedLnk> {
    let lnk = LnkFile::from_path(lnk_path)?;
    let basename = lnk_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| {
            FbuildError::PackageError(format!("invalid lnk path: {}", lnk_path.display()))
        })?;
    let stripped = basename.strip_suffix(".lnk").ok_or_else(|| {
        FbuildError::PackageError(format!(
            "lnk path does not end in .lnk: {}",
            lnk_path.display()
        ))
    })?;
    let target = materialized_root.join(stripped);
    materialize_one(lnk_path, &lnk, &target, cache)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fbuild_core::FbuildError;

    #[test]
    fn passes_through_non_lnk_entries() {
        let project = Path::new("/proj");
        let entries = vec!["data/file.bin".to_string(), "/abs/path/x.txt".to_string()];
        let resolved = expand_lnk_entries(&entries, project, |_| {
            panic!("should not be called for non-lnk entries")
        })
        .unwrap();
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0], Path::new("/proj/data/file.bin"));
        assert_eq!(resolved[1], Path::new("/abs/path/x.txt"));
    }

    #[test]
    fn invokes_resolver_for_lnk_entries() {
        let project = Path::new("/proj");
        let entries = vec!["data/asset.bin.lnk".to_string()];
        let mut calls = Vec::new();
        let resolved = expand_lnk_entries(&entries, project, |p| {
            calls.push(p.to_path_buf());
            Ok(PathBuf::from("/build/resources/data/asset.bin"))
        })
        .unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], Path::new("/proj/data/asset.bin.lnk"));
        assert_eq!(
            resolved,
            vec![PathBuf::from("/build/resources/data/asset.bin")]
        );
    }

    #[test]
    fn mixes_lnk_and_plain_entries_preserving_order() {
        let project = Path::new("/proj");
        let entries = vec![
            "a.bin".to_string(),
            "b.bin.lnk".to_string(),
            "c.bin".to_string(),
            "d.bin.lnk".to_string(),
        ];
        let mut counter = 0;
        let resolved = expand_lnk_entries(&entries, project, |_| {
            counter += 1;
            Ok(PathBuf::from(format!("/build/resolved-{counter}.bin")))
        })
        .unwrap();
        assert_eq!(resolved.len(), 4);
        assert_eq!(resolved[0], Path::new("/proj/a.bin"));
        assert_eq!(resolved[1], Path::new("/build/resolved-1.bin"));
        assert_eq!(resolved[2], Path::new("/proj/c.bin"));
        assert_eq!(resolved[3], Path::new("/build/resolved-2.bin"));
    }

    #[test]
    fn resolver_error_aborts_expansion() {
        let entries = vec!["good.bin.lnk".to_string(), "bad.bin.lnk".to_string()];
        let mut count = 0;
        let result = expand_lnk_entries(&entries, Path::new("/p"), |_| {
            count += 1;
            if count == 2 {
                Err(FbuildError::PackageError("simulated fetch failure".into()))
            } else {
                Ok(PathBuf::from("/ok"))
            }
        });
        let err = result.unwrap_err().to_string();
        assert!(err.contains("simulated fetch failure"), "got: {err}");
    }

    #[test]
    fn has_lnk_extension_handles_dotted_paths() {
        assert!(has_lnk_extension(Path::new("foo.lnk")));
        assert!(has_lnk_extension(Path::new("path/to/foo.bin.lnk")));
        assert!(!has_lnk_extension(Path::new("foo.lnk.bak")));
        assert!(!has_lnk_extension(Path::new("foo")));
        assert!(!has_lnk_extension(Path::new("foo.bin")));
    }
}
