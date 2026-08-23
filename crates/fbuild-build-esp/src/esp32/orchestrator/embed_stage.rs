//! Wrap `process_embed_files` with blob-pointer resolution + objcopy target
//! selection.

use std::path::{Path, PathBuf};

use fbuild_core::Result;

use super::super::mcu_config::Esp32McuConfig;
use super::embed::process_embed_files;

fn expand_embed_entries(
    entries: &[String],
    project_dir: &Path,
    lnk_dir: &Path,
    lnk_cache: Option<&fbuild_packages::DiskCache>,
    lnk_leases: &mut Vec<fbuild_packages::lnk::MaterializedLnk>,
) -> Result<Vec<String>> {
    let mut out = Vec::with_capacity(entries.len());
    // A materialized target is named after the pointer's blob, so two
    // pointers with the same blob name land on one path — the second
    // overwrites the first and both embed entries end up holding the second
    // blob's bytes. Refuse instead: a wrong asset embedded in firmware is
    // not something the user can see went wrong.
    // Keyed by `normalize_for_key`, not by `PathBuf` equality. Windows and
    // macOS are case-insensitive, so `logo.bin.fetch` and `LOGO.bin.lnk`
    // produce lexically distinct targets that are the *same file* — which is
    // exactly the collision this guard exists to catch, and the one a plain
    // comparison lets through.
    let mut claimed: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for entry in entries {
        let p = if Path::new(entry).is_absolute() {
            PathBuf::from(entry)
        } else {
            project_dir.join(entry)
        };
        if fbuild_packages::lnk::is_blob_pointer(&p) {
            let cache = lnk_cache.ok_or_else(|| {
                fbuild_core::FbuildError::PackageError(
                    "disk cache unavailable; cannot resolve blob-pointer (.fetch/.lnk) entries"
                        .to_string(),
                )
            })?;
            let materialized = fbuild_packages::lnk::materialize_lnk_entry(&p, lnk_dir, cache)?;
            let claim_key = fbuild_core::path::normalize_for_key(&materialized.target_path);
            if let Some(first) = claimed.insert(claim_key, entry.clone()) {
                return Err(fbuild_core::FbuildError::PackageError(format!(
                    "embed entries `{first}` and `{entry}` both materialize to {} — blob                      pointers are named after the blob they point at, so two of them cannot                      share one. Rename one, or drop the stale pointer if this is a leftover                      `.lnk` beside its `.fetch` replacement (FastLED/fbuild#1369).",
                    materialized.target_path.display()
                )));
            }
            out.push(materialized.target_path.to_string_lossy().into_owned());
            lnk_leases.push(materialized);
        } else {
            out.push(entry.clone());
        }
    }
    Ok(out)
}

/// Resolve blob-pointer entries in `embed_files`/`embed_txtfiles` against the disk
/// cache, then convert each entry into a linkable ELF object. Returns the
/// list of object files to be appended to the sketch link set.
#[allow(clippy::too_many_arguments)]
pub(super) async fn stage_embed_files(
    embed_files: &[String],
    embed_txtfiles: &[String],
    project_dir: &Path,
    build_dir: &Path,
    objcopy_path: &Path,
    mcu_config: &Esp32McuConfig,
    verbose: bool,
) -> Result<Vec<PathBuf>> {
    let embed_dir = build_dir.join("embed");
    std::fs::create_dir_all(&embed_dir)?;

    let lnk_dir = embed_dir.join("lnk");
    let mut lnk_leases: Vec<fbuild_packages::lnk::MaterializedLnk> = Vec::new();
    let lnk_cache = fbuild_packages::DiskCache::open().ok();

    let resolved_embed_files = expand_embed_entries(
        embed_files,
        project_dir,
        &lnk_dir,
        lnk_cache.as_ref(),
        &mut lnk_leases,
    )?;
    let resolved_embed_txtfiles = expand_embed_entries(
        embed_txtfiles,
        project_dir,
        &lnk_dir,
        lnk_cache.as_ref(),
        &mut lnk_leases,
    )?;

    let (output_target, binary_arch) = if mcu_config.is_riscv() {
        ("elf32-littleriscv", "riscv")
    } else {
        ("elf32-xtensa-le", "xtensa")
    };

    process_embed_files(
        &resolved_embed_files,
        &resolved_embed_txtfiles,
        project_dir,
        &embed_dir,
        objcopy_path,
        output_target,
        binary_arch,
        verbose,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use fbuild_packages::disk_cache::Kind;
    use sha2::{Digest, Sha256};

    #[test]
    fn materialized_lnk_lease_lives_while_embed_path_is_consumed() {
        let cache_root = tempfile::tempdir().unwrap();
        let cache = fbuild_packages::DiskCache::open_at(cache_root.path()).unwrap();
        let bytes = b"embedded cache content";
        let sha = format!("{:x}", Sha256::digest(bytes));
        let url = "https://localhost.invalid/embed.bin";

        let archive_dir = cache.archive_dir(Kind::LnkBlobs, url, &sha);
        std::fs::create_dir_all(&archive_dir).unwrap();
        let blob_path = archive_dir.join("embed.bin");
        std::fs::write(&blob_path, bytes).unwrap();
        cache
            .record_archive(
                Kind::LnkBlobs,
                url,
                &sha,
                &blob_path.to_string_lossy(),
                bytes.len() as i64,
                &sha,
            )
            .unwrap();

        let project = tempfile::tempdir().unwrap();
        let lnk_path = project.path().join("embed.bin.lnk");
        std::fs::write(
            &lnk_path,
            format!(r#"{{"v":1,"url":"{url}","sha256":"{sha}"}}"#),
        )
        .unwrap();

        let mut leases = Vec::new();
        let resolved = expand_embed_entries(
            &["embed.bin.lnk".to_string()],
            project.path(),
            &project.path().join("build/lnk"),
            Some(&cache),
            &mut leases,
        )
        .unwrap();

        let during_consumption = cache.lookup(Kind::LnkBlobs, url, &sha).unwrap().unwrap();
        assert_eq!(
            during_consumption.pinned, 1,
            "the cache lease must remain pinned while objcopy can consume the materialized path"
        );
        assert_eq!(std::fs::read(&resolved[0]).unwrap(), bytes);

        drop(leases);
        let after_operation = cache.lookup(Kind::LnkBlobs, url, &sha).unwrap().unwrap();
        assert_eq!(
            after_operation.pinned, 0,
            "the cache lease must release when embed processing ends"
        );
    }

    /// Two pointers whose blob names match materialize to one path, because
    /// the target is derived from the file name alone. The second silently
    /// replaced the first and both embed entries then pointed at the same
    /// bytes.
    ///
    /// Pre-existing for two `.lnk` in different directories; FastLED/fbuild
    /// #1369 adds the case where `foo.bin.fetch` and `foo.bin.lnk` sit in the
    /// *same* directory, which is exactly what a half-finished migration
    /// looks like. Silence is the wrong answer either way.
    #[test]
    fn colliding_blob_names_are_refused_rather_than_silently_overwritten() {
        let cache_root = tempfile::tempdir().unwrap();
        let cache = fbuild_packages::DiskCache::open_at(cache_root.path()).unwrap();
        let project = tempfile::tempdir().unwrap();

        let write_pointer = |rel: &str, body: &[u8]| {
            let sha = format!("{:x}", Sha256::digest(body));
            let url = format!("https://localhost.invalid/{rel}");
            let archive_dir = cache.archive_dir(Kind::LnkBlobs, &url, &sha);
            std::fs::create_dir_all(&archive_dir).unwrap();
            let blob_path = archive_dir.join("blob.bin");
            std::fs::write(&blob_path, body).unwrap();
            cache
                .record_archive(
                    Kind::LnkBlobs,
                    &url,
                    &sha,
                    &blob_path.to_string_lossy(),
                    body.len() as i64,
                    &sha,
                )
                .unwrap();
            let path = project.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                &path,
                format!(r#"{{"v":1,"url":"{url}","sha256":"{sha}"}}"#),
            )
            .unwrap();
        };
        write_pointer("logo.bin.fetch", b"the fetch blob");
        write_pointer("logo.bin.lnk", b"the legacy blob");

        let mut leases = Vec::new();
        let error = expand_embed_entries(
            &["logo.bin.fetch".to_string(), "logo.bin.lnk".to_string()],
            project.path(),
            &project.path().join("build/lnk"),
            Some(&cache),
            &mut leases,
        )
        .expect_err("two pointers cannot share one materialized path");
        let message = error.to_string();
        assert!(message.contains("logo.bin"), "{message}");
        assert!(
            message.contains("logo.bin.fetch") && message.contains("logo.bin.lnk"),
            "the error must name both pointers, or it is unactionable: {message}"
        );
    }

    /// FastLED/fbuild#1369 review: on Windows and macOS the filesystem folds
    /// case, so `logo.bin.fetch` and `LOGO.bin.lnk` materialize to one file
    /// while comparing unequal as paths. Keying the guard lexically let
    /// exactly the collision it was written to catch slip through — on the
    /// platforms where it actually happens.
    #[test]
    fn blob_names_differing_only_by_case_collide_on_case_insensitive_hosts() {
        if !fbuild_core::platform::host::is_windows() && !fbuild_core::platform::host::is_macos() {
            return; // case-sensitive host: these really are two distinct files
        }

        let cache_root = tempfile::tempdir().unwrap();
        let cache = fbuild_packages::DiskCache::open_at(cache_root.path()).unwrap();
        let project = tempfile::tempdir().unwrap();

        let write_pointer = |rel: &str, body: &[u8]| {
            let sha = format!("{:x}", Sha256::digest(body));
            let url = format!("https://localhost.invalid/{rel}");
            let archive_dir = cache.archive_dir(Kind::LnkBlobs, &url, &sha);
            std::fs::create_dir_all(&archive_dir).unwrap();
            let blob_path = archive_dir.join("blob.bin");
            std::fs::write(&blob_path, body).unwrap();
            cache
                .record_archive(
                    Kind::LnkBlobs,
                    &url,
                    &sha,
                    &blob_path.to_string_lossy(),
                    body.len() as i64,
                    &sha,
                )
                .unwrap();
            let path = project.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                &path,
                format!(r#"{{"v":1,"url":"{url}","sha256":"{sha}"}}"#),
            )
            .unwrap();
        };
        write_pointer("data/logo.bin.fetch", b"the fetch blob");
        write_pointer("assets/LOGO.bin.lnk", b"the legacy blob");

        let mut leases = Vec::new();
        let error = expand_embed_entries(
            &[
                "data/logo.bin.fetch".to_string(),
                "assets/LOGO.bin.lnk".to_string(),
            ],
            project.path(),
            &project.path().join("build/lnk"),
            Some(&cache),
            &mut leases,
        )
        .expect_err("case-folded names name one file on this host");
        assert!(error.to_string().contains("materialize to"), "{error}");
    }
}
