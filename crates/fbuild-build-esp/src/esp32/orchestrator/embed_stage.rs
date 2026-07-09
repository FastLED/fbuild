//! Wrap `process_embed_files` with `.lnk` resolution + objcopy target selection.

use std::path::{Path, PathBuf};

use fbuild_core::Result;

use super::super::mcu_config::Esp32McuConfig;
use super::embed::process_embed_files;

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
    let mut _lnk_leases: Vec<fbuild_packages::lnk::MaterializedLnk> = Vec::new();
    let lnk_cache = fbuild_packages::DiskCache::open().ok();

    let expand = |entries: &[String]| -> Result<Vec<String>> {
        let mut out = Vec::with_capacity(entries.len());
        for entry in entries {
            let p = if Path::new(entry).is_absolute() {
                PathBuf::from(entry)
            } else {
                project_dir.join(entry)
            };
            if fbuild_packages::lnk::has_lnk_extension(&p) {
                let cache = lnk_cache.as_ref().ok_or_else(|| {
                    fbuild_core::FbuildError::PackageError(
                        "disk cache unavailable; cannot resolve .lnk entries".to_string(),
                    )
                })?;
                let m = fbuild_packages::lnk::materialize_lnk_entry(&p, &lnk_dir, cache)?;
                out.push(m.target_path.to_string_lossy().into_owned());
            } else {
                out.push(entry.clone());
            }
        }
        Ok(out)
    };
    let resolved_embed_files = expand(embed_files)?;
    let resolved_embed_txtfiles = expand(embed_txtfiles)?;

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
