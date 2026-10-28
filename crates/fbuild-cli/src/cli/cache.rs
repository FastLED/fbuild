//! `fbuild cache save | restore | list | verify | reclaim` — first-class
//! archiving of the fbuild cache into a single portable `.tar.zst`, plus the
//! explicit low-disk recovery path.
//!
//! FastLED/fbuild#527. Replaces the fragile "cache these six paths"
//! `actions/cache@v4` workaround every consumer grows: one archive, one key,
//! and callers stop needing to know fbuild's internal cache layout.
//!
//! `reclaim` is the on-demand half of FastLED/fbuild#1750 item 3: it evicts
//! disposable caches in a fixed order (warm toolchains first, `.lnk` blobs
//! last) until the requested byte count is free, and **refuses to run while a
//! build is in progress** — #1341 was a GC evicting a warm toolchain mid-build.
//!
//! ```text
//! fbuild cache reclaim            # free the cache root's pre-flight shortfall
//! fbuild cache reclaim --needed 2G --dry-run
//! fbuild cache reclaim --needed 2G --cache-dir /mnt/big/fbuild
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
use fbuild_core::{FbuildError, Result};
use fbuild_packages::cache_archive::{self, SliceSelection};
use fbuild_packages::disk_cache::budget::parse_human_size;

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
    /// Free disk space on demand by evicting disposable caches in a fixed
    /// order (FastLED/fbuild#1750 item 3). Refuses to run while a build is in
    /// progress; exits non-zero when the target could not be met.
    Reclaim {
        /// Bytes to free, human-readable (`2G`, `512M`, `1048576`). Defaults to
        /// the cache root's pre-flight shortfall.
        #[arg(long)]
        needed: Option<String>,
        /// Report what would be evicted without deleting anything.
        #[arg(long)]
        dry_run: bool,
        /// Override the cache root (defaults to fbuild's dev/prod cache dir).
        #[arg(long)]
        cache_dir: Option<PathBuf>,
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
        CacheAction::Reclaim {
            needed,
            dry_run,
            cache_dir,
        } => reclaim_cache(needed.as_deref(), dry_run, cache_dir),
    }
}

/// `fbuild cache reclaim` — evict disposable caches until `needed` bytes are
/// free (FastLED/fbuild#1750 item 3).
///
/// Three refusals, each deliberate: a build in progress (#1341: never evict
/// mid-build), a `--needed` that is not a byte count, and an unmeasurable cache
/// volume when the target has to be derived. Everything else is best effort and
/// reports bytes freed, not a promise.
fn reclaim_cache(needed: Option<&str>, dry_run: bool, cache_dir: Option<PathBuf>) -> Result<()> {
    if let Some(reason) = build_in_progress() {
        return Err(FbuildError::BuildFailed(format!(
            "refusing to reclaim the cache: a build is in progress ({reason}). \
             Reclaim never runs mid-build — the daemon's background GC evicting a \
             warm toolchain under a running build is FastLED/fbuild#1341."
        )));
    }

    let root = cache_dir.unwrap_or_else(fbuild_paths::get_cache_root);
    let needed_bytes = match needed {
        Some(raw) => parse_human_size(raw).ok_or_else(|| {
            FbuildError::BuildFailed(format!(
                "--needed '{raw}' is not a byte count (try 2G, 512M or 1048576)"
            ))
        })?,
        None => default_needed_bytes(&root)?,
    };

    if needed_bytes == 0 {
        output::result(format!(
            "cache reclaim: nothing to do — {} already satisfies the ESP32 cache \
             requirement; pass --needed <bytes> to evict anyway",
            root.display()
        ));
        return Ok(());
    }

    let cache = fbuild_packages::DiskCache::open_at(&root).map_err(|error| {
        FbuildError::BuildFailed(format!("cache index at {}: {error}", root.display()))
    })?;
    let report = cache
        .reclaim(needed_bytes, dry_run)
        .map_err(|error| FbuildError::BuildFailed(format!("reclaim failed: {error}")))?;

    let text = render_reclaim(&report, &root);
    output::result(text.trim_end());
    if report.is_satisfied() {
        return Ok(());
    }
    Err(FbuildError::BuildFailed(format!(
        "reclaim fell short: {} still missing after evicting every reclaimable \
         disk-cache entry under {}",
        human_bytes(report.remaining_bytes()),
        root.display()
    )))
}

/// The target when `--needed` is absent: the cache root's pre-flight shortfall,
/// measured by the same seam the build enforces (`disk_preflight`).
fn default_needed_bytes(root: &std::path::Path) -> Result<u64> {
    let required = fbuild_build::esp32::orchestrator::disk_requirements::cache_requirement();
    if required == 0 {
        return Err(FbuildError::BuildFailed(
            "FBUILD_MIN_DISK_BYTES=0 disables the cache requirement, so there is no \
             default target — pass --needed <bytes>"
                .to_string(),
        ));
    }
    let roots = [fbuild_build::disk_preflight::DiskRoot::new(
        "cache", root, required,
    )];
    let report = fbuild_build::disk_preflight::measure(
        &fbuild_build::disk_preflight::HostFreeSpace
            as &dyn fbuild_build::disk_preflight::FreeSpace,
        &roots,
    );
    let measured = report.roots().first().ok_or_else(|| {
        FbuildError::BuildFailed(format!(
            "could not measure {} to derive a target; pass --needed <bytes>",
            root.display()
        ))
    })?;
    Ok(measured.shortfall())
}

/// Why reclaim must not run right now, from the daemon status file.
///
/// `None` means no build is in progress — including when the file is missing or
/// unparseable: a host with no daemon has nothing to evict out from under.
fn build_in_progress() -> Option<String> {
    let path = fbuild_paths::get_daemon_status_file();
    let text = std::fs::read_to_string(path).ok()?;
    busy_reason(&text)
}

/// Pure form of [`build_in_progress`]: the reason from a `daemon_status.json`
/// body, or `None` when the daemon reports no operation in progress.
pub fn busy_reason(status_json: &str) -> Option<String> {
    let status: serde_json::Value = serde_json::from_str(status_json).ok()?;
    if !status.get("operation_in_progress")?.as_bool()? {
        return None;
    }
    let operation = status
        .get("current_operation")
        .and_then(|value| value.as_str())
        .unwrap_or("a build");
    let message = status
        .get("message")
        .and_then(|value| value.as_str())
        .unwrap_or("no detail");
    Some(format!("{operation}: {message}"))
}

/// Render a reclaim run: one line per step in eviction order, then the verdict.
fn render_reclaim(
    report: &fbuild_packages::disk_cache::reclaim::ReclaimReport,
    cache_root: &std::path::Path,
) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "cache reclaim{}: needed {}, freed {} ({} {} evicted)",
        if report.dry_run() {
            " (dry run — nothing deleted)"
        } else {
            ""
        },
        human_bytes(report.needed_bytes()),
        human_bytes(report.freed_bytes()),
        report.evicted(),
        if report.evicted() == 1 {
            "entry"
        } else {
            "entries"
        },
    );
    for step in report.steps() {
        let _ = writeln!(
            out,
            "  {:<12} {:>4} entry(s)  {:>10}  (installed {} × {}, archives {} × {})",
            step.kind.as_str(),
            step.evicted(),
            human_bytes(step.freed_bytes()),
            step.installed_evicted,
            human_bytes(step.installed_bytes_freed),
            step.archives_evicted,
            human_bytes(step.archive_bytes_freed),
        );
    }
    if report.steps().is_empty() {
        let _ = writeln!(out, "  nothing reclaimable under {}", cache_root.display());
    }
    if report.is_satisfied() {
        let _ = writeln!(
            out,
            "verdict: satisfied — {} needed, {} freed",
            human_bytes(report.needed_bytes()),
            human_bytes(report.freed_bytes())
        );
        return out;
    }
    let _ = writeln!(
        out,
        "verdict: SHORT by {} — every reclaimable disk-cache entry is gone",
        human_bytes(report.remaining_bytes())
    );
    // The zccache object cache is the last step of #1750's order and the only
    // one fbuild cannot drive from here: its LRU budget belongs to the embedded
    // zccache service inside fbuild-daemon (no cross-process trim API).
    let zccache = fbuild_paths::get_fbuild_root().join("zccache");
    let _ = writeln!(
        out,
        "  zccache      not evicted here: its LRU budget is enforced by the daemon \
         ({}) — run `fbuild purge` or start an idle daemon to let it trim",
        zccache.display()
    );
    out
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
            "  {:<18} {:>6} file(s)  {:>10}  {}",
            s.name,
            s.file_count,
            human_bytes(s.byte_count),
            &s.content_hash[..s.content_hash.len().min(12)]
        );
    }
    let _ = write!(
        out,
        "  {:<18} {:>6} file(s)  {:>10}",
        "TOTAL",
        m.total_files(),
        human_bytes(m.total_bytes())
    );
    out
}

pub(crate) fn human_bytes(n: u64) -> String {
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

#[cfg(test)]
#[path = "cache_reclaim_tests.rs"]
mod reclaim_tests;
