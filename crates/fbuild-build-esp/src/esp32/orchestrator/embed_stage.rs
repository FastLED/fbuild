//! Wrap `process_embed_files` with `.lnk` resolution + objcopy target selection.

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
    for entry in entries {
        let p = if Path::new(entry).is_absolute() {
            PathBuf::from(entry)
        } else {
            project_dir.join(entry)
        };
        if fbuild_packages::lnk::has_lnk_extension(&p) {
            let cache = lnk_cache.ok_or_else(|| {
                fbuild_core::FbuildError::PackageError(
                    "disk cache unavailable; cannot resolve .lnk entries".to_string(),
                )
            })?;
            let materialized = fbuild_packages::lnk::materialize_lnk_entry(&p, lnk_dir, cache)?;
            out.push(materialized.target_path.to_string_lossy().into_owned());
            lnk_leases.push(materialized);
        } else {
            out.push(entry.clone());
        }
    }
    Ok(out)
}

/// Resolve `.lnk` entries in `embed_files`/`embed_txtfiles` against the disk
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
}
