//! Scan a source tree for `.lnk` files.
//!
//! `scan_for_lnk(root)` walks the directory tree at `root`, finds every
//! file ending in `.lnk`, parses each as a `LnkFile`, and returns the
//! `(path, parsed)` pairs. Parse errors are logged but do not abort the
//! scan — one malformed `.lnk` shouldn't kill the whole build setup.
//!
//! Symlinks are followed so users can stash `.lnk` files in shared
//! directories, but cycle detection is left to `walkdir`.

use std::path::{Path, PathBuf};

use fbuild_core::Result;
use tracing::warn;
use walkdir::WalkDir;

use super::format::LnkFile;

/// A `.lnk` file discovered on disk and successfully parsed.
#[derive(Debug, Clone)]
pub struct DiscoveredLnk {
    /// Absolute path to the `.lnk` file in the source tree.
    pub path: PathBuf,
    /// Parsed manifest contents.
    pub lnk: LnkFile,
}

/// Walk `root` recursively and return every `.lnk` file that parses cleanly.
///
/// Files that fail to parse are logged at WARN level and skipped. The
/// returned vector is unsorted (caller orders if needed).
///
/// Returns `Err` only on irrecoverable I/O — e.g. `root` does not exist.
pub fn scan_for_lnk(root: &Path) -> Result<Vec<DiscoveredLnk>> {
    let mut out = Vec::new();
    if !root.exists() {
        return Ok(out);
    }

    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if !super::is_blob_pointer(path) {
            continue;
        }
        match LnkFile::from_path(path) {
            Ok(lnk) => out.push(DiscoveredLnk {
                path: path.to_path_buf(),
                lnk,
            }),
            Err(e) => {
                // FastLED/fbuild#1369: a `.lnk` that will not parse as JSON
                // is more likely FastLED's runtime asset link — plain text,
                // read on the MCU by `fl::parse_lnk`, and none of fbuild's
                // business — than a corrupt blob pointer. Say so, instead of
                // calling someone's correct file malformed. A `.fetch` is
                // unambiguously ours, so that one keeps the blunt wording.
                if path.extension().and_then(|s| s.to_str())
                    == Some(super::LEGACY_BLOB_POINTER_EXTENSION)
                {
                    warn!(
                        path = %path.display(),
                        error = %e,
                        "skipping .lnk that is not fbuild's JSON blob-pointer format; if this is                          a runtime asset link consumed by fl::parse_lnk, it is not fbuild's to                          resolve and this warning is expected"
                    );
                } else {
                    warn!(
                        path = %path.display(),
                        error = %e,
                        "skipping malformed blob pointer"
                    );
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const VALID_SHA: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

    fn write_valid_lnk(path: &Path, url: &str) {
        let json = format!(r#"{{"v":1,"url":"{url}","sha256":"{VALID_SHA}"}}"#);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, json).unwrap();
    }

    #[test]
    fn empty_dir_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let found = scan_for_lnk(dir.path()).unwrap();
        assert!(found.is_empty());
    }

    #[test]
    fn nonexistent_root_returns_empty() {
        let found = scan_for_lnk(Path::new("/this/path/does/not/exist/xyz")).unwrap();
        assert!(found.is_empty());
    }

    #[test]
    fn finds_top_level_lnk() {
        let dir = tempfile::tempdir().unwrap();
        write_valid_lnk(&dir.path().join("foo.bin.lnk"), "https://x/foo.bin");
        let found = scan_for_lnk(dir.path()).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].lnk.url, "https://x/foo.bin");
    }

    #[test]
    fn finds_nested_lnks() {
        let dir = tempfile::tempdir().unwrap();
        write_valid_lnk(&dir.path().join("a/b/c/x.bin.lnk"), "https://x/a.bin");
        write_valid_lnk(&dir.path().join("a/y.bin.lnk"), "https://x/y.bin");
        write_valid_lnk(&dir.path().join("z.bin.lnk"), "https://x/z.bin");
        let mut found = scan_for_lnk(dir.path()).unwrap();
        found.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(found.len(), 3);
    }

    #[test]
    fn ignores_non_lnk_files() {
        let dir = tempfile::tempdir().unwrap();
        write_valid_lnk(&dir.path().join("real.bin.lnk"), "https://x/r.bin");
        fs::write(dir.path().join("not_a_link.txt"), "some text").unwrap();
        fs::write(dir.path().join("also_not.lnk.bak"), "{}").unwrap();
        let found = scan_for_lnk(dir.path()).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].lnk.url, "https://x/r.bin");
    }

    #[test]
    fn malformed_lnk_is_skipped_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        write_valid_lnk(&dir.path().join("good.bin.lnk"), "https://x/g.bin");
        fs::write(dir.path().join("bad.bin.lnk"), "{not valid json}").unwrap();
        let found = scan_for_lnk(dir.path()).unwrap();
        // The good one is found; the bad one is logged + skipped.
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].lnk.url, "https://x/g.bin");
    }

    /// FastLED/fbuild#1369: the scanner must find what `fbuild lnk add`
    /// now writes, and must go on finding what it wrote before.
    #[test]
    fn finds_both_pointer_extensions_in_one_tree() {
        let dir = tempfile::tempdir().unwrap();
        write_valid_lnk(&dir.path().join("new.bin.fetch"), "https://x/new.bin");
        write_valid_lnk(&dir.path().join("old.bin.lnk"), "https://x/old.bin");
        let mut found: Vec<String> = scan_for_lnk(dir.path())
            .unwrap()
            .into_iter()
            .map(|d| d.lnk.url)
            .collect();
        found.sort();
        assert_eq!(found, vec!["https://x/new.bin", "https://x/old.bin"]);
    }

    #[test]
    fn directory_with_lnk_extension_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        // Pathological: a *directory* named foo.lnk. Should be skipped because
        // it's not a file, not because of the extension.
        fs::create_dir_all(dir.path().join("weird.lnk")).unwrap();
        write_valid_lnk(&dir.path().join("real.bin.lnk"), "https://x/r.bin");
        let found = scan_for_lnk(dir.path()).unwrap();
        assert_eq!(found.len(), 1);
    }
}
