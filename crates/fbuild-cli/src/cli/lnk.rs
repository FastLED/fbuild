//! `fbuild lnk` subcommands.
//!
//! - `pull`  — scan + fetch every .lnk's blob into the disk cache
//! - `check` — verify every cached blob's sha256 (no network)
//! - `add`   — fetch a URL once, hash it, write a new .lnk pointing at it

use crate::output;

use super::args::LnkAction;

pub async fn run_lnk(
    action: LnkAction,
    top_level_project_dir: &Option<String>,
) -> fbuild_core::Result<()> {
    use std::io::Write;
    use std::path::PathBuf;

    use fbuild_packages::lnk::{ExtractMode, LnkFile, scan_for_lnk};
    use sha2::{Digest, Sha256};

    fn open_cache() -> fbuild_core::Result<fbuild_packages::DiskCache> {
        fbuild_packages::DiskCache::open().map_err(|e| {
            fbuild_core::FbuildError::PackageError(format!("failed to open lnk disk cache: {e}"))
        })
    }

    fn resolve_root(explicit: Option<String>, fallback: &Option<String>) -> PathBuf {
        let chosen = explicit
            .or_else(|| fallback.clone())
            .unwrap_or_else(|| ".".to_string());
        PathBuf::from(chosen)
    }

    match action {
        LnkAction::Pull { project_dir } => {
            let root = resolve_root(project_dir, top_level_project_dir);
            let discovered = scan_for_lnk(&root)?;
            if discovered.is_empty() {
                output::result(format!("no .lnk files found under {}", root.display()));
                return Ok(());
            }
            let cache = open_cache()?;
            let mut ok = 0usize;
            let mut failed = 0usize;
            for d in &discovered {
                match fbuild_packages::lnk::resolve(&d.lnk, &cache) {
                    Ok(r) => {
                        ok += 1;
                        output::result(format!(
                            "ok   {}  →  {}  ({})",
                            d.path.display(),
                            r.path.display(),
                            d.lnk.sha256
                        ));
                    }
                    Err(e) => {
                        failed += 1;
                        output::error(format!("FAIL {}: {}", d.path.display(), e));
                    }
                }
            }
            output::result(format!(
                "\nlnk pull: {ok} ok, {failed} failed (of {})",
                discovered.len()
            ));
            if failed > 0 {
                std::process::exit(1);
            }
            Ok(())
        }

        LnkAction::Check { project_dir } => {
            let root = resolve_root(project_dir, top_level_project_dir);
            let discovered = scan_for_lnk(&root)?;
            if discovered.is_empty() {
                output::result(format!("no .lnk files found under {}", root.display()));
                return Ok(());
            }
            let cache = open_cache()?;
            let mut ok = 0usize;
            let mut missing = 0usize;
            let mut mismatched = 0usize;
            for d in &discovered {
                let entry = cache
                    .lookup(
                        fbuild_packages::disk_cache::Kind::LnkBlobs,
                        &d.lnk.url,
                        &d.lnk.sha256,
                    )
                    .map_err(|e| {
                        fbuild_core::FbuildError::PackageError(format!(
                            "lnk cache lookup failed for {}: {e}",
                            d.path.display()
                        ))
                    })?;
                let Some(entry) = entry else {
                    missing += 1;
                    output::result(format!(
                        "MISSING {}  (run `fbuild lnk pull` to fetch)",
                        d.path.display()
                    ));
                    continue;
                };
                let blob_path = PathBuf::from(entry.archive_path.unwrap_or_default());
                if !blob_path.exists() {
                    missing += 1;
                    output::result(format!(
                        "MISSING {}  (cache index points at {} which is gone)",
                        d.path.display(),
                        blob_path.display()
                    ));
                    continue;
                }
                let bytes = std::fs::read(&blob_path).map_err(|e| {
                    fbuild_core::FbuildError::PackageError(format!(
                        "failed to read {}: {e}",
                        blob_path.display()
                    ))
                })?;
                let mut h = Sha256::new();
                h.update(&bytes);
                let actual = format!("{:x}", h.finalize());
                if actual == d.lnk.sha256 {
                    ok += 1;
                    output::result(format!("ok       {}", d.path.display()));
                } else {
                    mismatched += 1;
                    output::result(format!(
                        "BAD      {}  (expected {}, got {})",
                        d.path.display(),
                        d.lnk.sha256,
                        actual
                    ));
                }
            }
            output::result(format!(
                "\nlnk check: {ok} ok, {missing} missing, {mismatched} mismatched (of {})",
                discovered.len()
            ));
            if mismatched > 0 || missing > 0 {
                std::process::exit(1);
            }
            Ok(())
        }

        LnkAction::Add {
            url,
            output: output_arg,
        } => {
            // Determine output path before downloading so we fail early on a
            // bad output spec.
            let basename = url.rsplit('/').next().unwrap_or("blob");
            let output_path = match output_arg {
                Some(p) => PathBuf::from(p),
                None => PathBuf::from(format!("{basename}.lnk")),
            };
            if let Some(parent) = output_path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        fbuild_core::FbuildError::PackageError(format!(
                            "failed to create {}: {e}",
                            parent.display()
                        ))
                    })?;
                }
            }

            // Download to a temp dir, hash it, then write the .lnk.
            // Rooted under `~/.fbuild/{dev|prod}/tmp/lnk-download/` —
            // FastLED/fbuild#844 bridge pair 10.
            let tmp =
                tempfile::tempdir_in(fbuild_paths::temp_subdir("lnk-download")).map_err(|e| {
                    fbuild_core::FbuildError::PackageError(format!(
                        "failed to create temp dir: {e}"
                    ))
                })?;
            let downloaded = fbuild_packages::downloader::download_file(&url, tmp.path()).await?;
            let bytes = std::fs::read(&downloaded).map_err(|e| {
                fbuild_core::FbuildError::PackageError(format!(
                    "failed to read downloaded file: {e}"
                ))
            })?;
            let mut h = Sha256::new();
            h.update(&bytes);
            let sha = format!("{:x}", h.finalize());

            // Round-trip through serde so the format matches what the
            // parser accepts. Also ensures a v=1 wrapper.
            let lnk = LnkFile {
                version: 1,
                url: url.clone(),
                sha256: sha.clone(),
                size: Some(bytes.len() as u64),
                extract: ExtractMode::File,
            };
            let json = serde_json::json!({
                "v": lnk.version,
                "url": lnk.url,
                "sha256": lnk.sha256,
                "size": lnk.size,
            });
            let pretty = serde_json::to_string_pretty(&json).map_err(|e| {
                fbuild_core::FbuildError::PackageError(format!(
                    "failed to serialize .lnk JSON: {e}"
                ))
            })?;
            let mut f = std::fs::File::create(&output_path).map_err(|e| {
                fbuild_core::FbuildError::PackageError(format!(
                    "failed to create {}: {e}",
                    output_path.display()
                ))
            })?;
            f.write_all(pretty.as_bytes()).map_err(|e| {
                fbuild_core::FbuildError::PackageError(format!(
                    "failed to write {}: {e}",
                    output_path.display()
                ))
            })?;
            f.write_all(b"\n").ok();

            output::result(format!(
                "wrote {}  ({} bytes, sha256={})",
                output_path.display(),
                bytes.len(),
                sha
            ));
            Ok(())
        }
    }
}
