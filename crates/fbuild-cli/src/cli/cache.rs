//! `fbuild cache save | restore | list | verify` — first-class archiving of
//! the fbuild cache into a single portable `.tar.zst`.
//!
//! FastLED/fbuild#527. Replaces the fragile "cache these six paths"
//! `actions/cache@v4` workaround every consumer grows: one archive, one key,
//! and callers stop needing to know fbuild's internal cache layout.
//!
//! ```text
//! fbuild cache save    ~/fbuild-cache.tar.zst
//! fbuild build . -e <env>
//! fbuild cache restore ~/fbuild-cache.tar.zst
//! fbuild cache list    ~/fbuild-cache.tar.zst
//! fbuild cache verify  ~/fbuild-cache.tar.zst
//! ```
//!
//! This is an in-process diagnostic (no daemon round-trip) — it only touches
//! paths + packaging, like `port scan` / `lnk`.

use std::path::PathBuf;

use clap::Subcommand;
use fbuild_core::Result;
use fbuild_packages::cache_archive::{self, SliceSelection};

use crate::output;

#[derive(Subcommand)]
pub enum CacheAction {
    /// Pack the fbuild cache into `<archive>` (`.tar.zst`).
    Save {
        /// Destination archive path.
        archive: PathBuf,
        /// Comma-separated slice names to include (default: everything fbuild
        /// owns under the cache root). Run `cache list` on any archive or see
        /// `--help` for names.
        #[arg(long, value_delimiter = ',')]
        include: Vec<String>,
        /// Comma-separated slice names to subtract from the include set.
        #[arg(long, value_delimiter = ',')]
        exclude: Vec<String>,
        /// zstd compression level (1..=22, default 9).
        #[arg(long, default_value_t = cache_archive::DEFAULT_ZSTD_LEVEL)]
        zstd_level: i32,
        /// Override the cache root (defaults to fbuild's dev/prod cache dir).
        #[arg(long)]
        cache_dir: Option<PathBuf>,
    },
    /// Restore a `.tar.zst` archive back into the fbuild cache.
    Restore {
        /// Archive to restore.
        archive: PathBuf,
        /// Override the cache root (defaults to fbuild's dev/prod cache dir).
        #[arg(long)]
        cache_dir: Option<PathBuf>,
    },
    /// Print the archive's manifest (slices, file/byte counts, hashes) without
    /// extracting anything.
    List {
        /// Archive to inspect.
        archive: PathBuf,
    },
    /// Verify archive integrity: re-hash every slice's payload against the
    /// manifest. Exits non-zero on mismatch.
    Verify {
        /// Archive to verify.
        archive: PathBuf,
    },
}

/// Dispatcher entry — called from `cli::dispatch`.
pub fn run_cache(action: CacheAction) -> Result<()> {
    match action {
        CacheAction::Save {
            archive,
            include,
            exclude,
            zstd_level,
            cache_dir,
        } => {
            let root = cache_dir.unwrap_or_else(fbuild_paths::get_cache_root);
            let selection = if include.is_empty() {
                SliceSelection::Default
            } else {
                SliceSelection::Explicit(include)
            };
            let manifest = cache_archive::save(&root, &archive, &selection, &exclude, zstd_level)?;
            output::result(render_saved(&archive, &manifest));
            Ok(())
        }
        CacheAction::Restore { archive, cache_dir } => {
            let root = cache_dir.unwrap_or_else(fbuild_paths::get_cache_root);
            let manifest = cache_archive::restore(&archive, &root)?;
            output::result(format!(
                "restored {} slice(s), {} file(s), {} into {}",
                manifest.slices.len(),
                manifest.total_files(),
                human_bytes(manifest.total_bytes()),
                root.display()
            ));
            Ok(())
        }
        CacheAction::List { archive } => {
            let manifest = cache_archive::read_manifest(&archive)?;
            output::result(render_manifest(&archive, &manifest, false));
            Ok(())
        }
        CacheAction::Verify { archive } => {
            let manifest = cache_archive::verify(&archive)?;
            output::result(render_manifest(&archive, &manifest, true));
            Ok(())
        }
    }
}

fn render_saved(archive: &std::path::Path, m: &cache_archive::CacheManifest) -> String {
    let on_disk = std::fs::metadata(archive).map(|md| md.len()).unwrap_or(0);
    format!(
        "saved {} slice(s), {} file(s), {} → {} ({} compressed)",
        m.slices.len(),
        m.total_files(),
        human_bytes(m.total_bytes()),
        archive.display(),
        human_bytes(on_disk),
    )
}

fn render_manifest(
    archive: &std::path::Path,
    m: &cache_archive::CacheManifest,
    verified: bool,
) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} (fbuild v{}, format v{}){}",
        archive.display(),
        m.fbuild_version,
        m.format_version,
        if verified { " — VERIFIED OK" } else { "" }
    );
    for s in &m.slices {
        let _ = writeln!(
            out,
            "  {:<12} {:>6} file(s)  {:>10}  {}",
            s.name,
            s.file_count,
            human_bytes(s.byte_count),
            &s.content_hash[..s.content_hash.len().min(12)]
        );
    }
    let _ = write!(
        out,
        "  {:<12} {:>6} file(s)  {:>10}",
        "TOTAL",
        m.total_files(),
        human_bytes(m.total_bytes())
    );
    out
}

fn human_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_bytes_scales() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1.0 KB");
        assert_eq!(human_bytes(1024 * 1024 * 3), "3.0 MB");
    }

    #[test]
    fn manifest_render_lists_slices_and_total() {
        let m = cache_archive::CacheManifest {
            format_version: 1,
            fbuild_version: "9.9.9".into(),
            slices: vec![cache_archive::SliceInfo {
                name: "toolchains".into(),
                file_count: 3,
                byte_count: 2048,
                content_hash: "abcdef0123456789".into(),
            }],
        };
        let s = render_manifest(std::path::Path::new("x.tar.zst"), &m, true);
        assert!(s.contains("VERIFIED OK"));
        assert!(s.contains("toolchains"));
        assert!(s.contains("TOTAL"));
        assert!(s.contains("fbuild v9.9.9"));
    }

    #[test]
    fn save_then_list_roundtrips_through_cli_layer() {
        let src = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(src.path().join("toolchains")).unwrap();
        std::fs::write(src.path().join("toolchains/gcc"), "GCC").unwrap();
        std::fs::write(src.path().join("index.sqlite"), "DB").unwrap();
        let archive = src.path().join("c.tar.zst");

        run_cache(CacheAction::Save {
            archive: archive.clone(),
            include: vec![],
            exclude: vec![],
            zstd_level: 3,
            cache_dir: Some(src.path().to_path_buf()),
        })
        .unwrap();
        assert!(archive.is_file());

        // list + verify go through the CLI dispatcher without error.
        run_cache(CacheAction::List {
            archive: archive.clone(),
        })
        .unwrap();
        run_cache(CacheAction::Verify {
            archive: archive.clone(),
        })
        .unwrap();

        // restore into a fresh dir reproduces the file.
        let dst = tempfile::tempdir().unwrap();
        run_cache(CacheAction::Restore {
            archive,
            cache_dir: Some(dst.path().to_path_buf()),
        })
        .unwrap();
        assert_eq!(
            std::fs::read(dst.path().join("toolchains/gcc")).unwrap(),
            b"GCC"
        );
    }
}
