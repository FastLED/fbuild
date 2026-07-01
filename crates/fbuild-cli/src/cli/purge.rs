//! `fbuild purge` and `fbuild purge --gc` handlers plus the byte/size
//! formatting helpers they share with the daemon subcommands.

use crate::daemon_client::DaemonClient;
use crate::output;

use super::daemon_cmd::print_gc_report;

pub async fn run_purge_gc() -> fbuild_core::Result<()> {
    // Try to route GC through the daemon to respect its gc_mutex.
    let client = DaemonClient::new();
    if client.health().await {
        let result = client.run_gc().await?;
        if !result.success {
            return Err(fbuild_core::FbuildError::Other(format!(
                "GC failed: {}",
                result.message.as_deref().unwrap_or("unknown error")
            )));
        }
        output::result("GC complete (via daemon):");
        output::result(format!(
            "  Installed evicted: {} ({})",
            result.installed_evicted,
            format_size(result.installed_bytes_freed)
        ));
        output::result(format!(
            "  Archives evicted:  {} ({})",
            result.archives_evicted,
            format_size(result.archive_bytes_freed)
        ));
        output::result(format!(
            "  Total freed:       {}",
            format_size(result.total_bytes_freed)
        ));
        if result.orphan_files_removed > 0 {
            output::result(format!(
                "  Orphan files removed: {}",
                result.orphan_files_removed
            ));
        }
        if result.orphan_rows_cleaned > 0 {
            output::result(format!(
                "  Orphan rows cleaned:  {}",
                result.orphan_rows_cleaned
            ));
        }
        return Ok(());
    }

    // No daemon running — safe to run GC locally.
    match fbuild_packages::DiskCache::open() {
        Ok(dc) => match dc.run_gc() {
            Ok(report) => {
                print_gc_report(&report);
                Ok(())
            }
            Err(e) => Err(fbuild_core::FbuildError::Other(format!("GC failed: {}", e))),
        },
        Err(e) => Err(fbuild_core::FbuildError::Other(format!(
            "failed to open disk cache: {}",
            e
        ))),
    }
}

pub fn run_purge(
    target: Option<String>,
    dry_run: bool,
    project_dir: Option<String>,
) -> fbuild_core::Result<()> {
    let cache_root = fbuild_paths::get_cache_root();

    match target.as_deref() {
        None => {
            // No target: list cached packages (matches Python behavior)
            list_cached_packages(&cache_root)?;
            std::process::exit(1);
        }
        Some("all") => {
            // Purge entire global cache
            purge_dir(&cache_root, dry_run)?;
        }
        Some("project") => {
            // Purge project-local .fbuild/ directory
            let pd = project_dir.as_deref().unwrap_or(".");
            let fbuild_dir = fbuild_paths::get_project_fbuild_dir(std::path::Path::new(pd));
            purge_dir(&fbuild_dir, dry_run)?;
        }
        Some(t) => {
            // Purge specific cache subdirectory (e.g., environment name)
            let path = cache_root.join(t);
            if !path.exists() {
                output::error(format!("target not found: {}", path.display()));
                return Ok(());
            }
            purge_dir(&path, dry_run)?;
        }
    }
    Ok(())
}

pub fn purge_dir(path: &std::path::Path, dry_run: bool) -> fbuild_core::Result<()> {
    if !path.exists() {
        output::result(format!("nothing to purge: {}", path.display()));
        return Ok(());
    }
    let size = dir_size(path);
    if dry_run {
        output::result(format!(
            "would remove: {} ({})",
            path.display(),
            format_size(size)
        ));
    } else {
        std::fs::remove_dir_all(path).map_err(|e| {
            fbuild_core::FbuildError::Other(format!("failed to remove {}: {}", path.display(), e))
        })?;
        output::result(format!(
            "removed: {} ({})",
            path.display(),
            format_size(size)
        ));
    }
    Ok(())
}

pub fn list_cached_packages(cache_root: &std::path::Path) -> fbuild_core::Result<()> {
    if !cache_root.exists() {
        output::result(format!(
            "No cached packages found at {}",
            cache_root.display()
        ));
        output::result("\nUsage:");
        output::result("  fbuild purge all              Remove all cached packages");
        output::result("  fbuild purge project          Remove project build artifacts (.fbuild/)");
        output::result("  fbuild purge <name>           Remove specific cache subdirectory");
        output::result("  fbuild purge ... --dry-run    Show what would be removed");
        return Ok(());
    }

    let mut total_size: u64 = 0;
    let mut total_count: usize = 0;

    // Walk top-level type directories (toolchains, platforms, frameworks, etc.)
    let mut entries: Vec<_> = std::fs::read_dir(cache_root)
        .map_err(|e| fbuild_core::FbuildError::Other(format!("failed to read cache dir: {}", e)))?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for type_entry in entries {
        let type_name = type_entry.file_name();
        let type_path = type_entry.path();

        // Collect packages within this type directory
        let mut packages: Vec<(String, u64)> = Vec::new();
        if let Ok(subdirs) = std::fs::read_dir(&type_path) {
            for sub in subdirs.filter_map(|e| e.ok()) {
                let sub_path = sub.path();
                if sub_path.is_dir() {
                    let name = sub.file_name().to_string_lossy().to_string();
                    let size = dir_size(&sub_path);
                    packages.push((name, size));
                }
            }
        }
        packages.sort_by(|a, b| a.0.cmp(&b.0));

        if !packages.is_empty() {
            output::result(format!("{}:", type_name.to_string_lossy().to_uppercase()));
            for (name, size) in &packages {
                output::result(format!("  {} ({})", name, format_size(*size)));
                total_size += size;
                total_count += 1;
            }
            output::result("");
        }
    }

    output::result(format!(
        "Total: {} package(s), {}",
        total_count,
        format_size(total_size)
    ));
    output::result(
        "\nUse 'fbuild purge all' to remove all, or 'fbuild purge <target>' for specific.",
    );
    Ok(())
}

pub fn dir_size(path: &std::path::Path) -> u64 {
    let mut size = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.filter_map(|e| e.ok()) {
            let p = entry.path();
            if p.is_dir() {
                size += dir_size(&p);
            } else if let Ok(meta) = p.metadata() {
                size += meta.len();
            }
        }
    }
    size
}

pub fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}
