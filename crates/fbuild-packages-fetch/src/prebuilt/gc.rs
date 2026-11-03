//! GC and fetch registration share one in-memory mutex; no live partial is reaped.
use super::partial::{Meta, now};
use std::{collections::HashSet, path::Path, time::Duration};
pub fn collect(root: &Path, live: &HashSet<String>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(root.join(".staging"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        let sha = name.split('.').next().unwrap_or_default();
        if !live.contains(sha) {
            std::fs::remove_dir_all(entry.path())?;
        }
    }
    let mut partials = Vec::new();
    let mut total = 0u64;
    for entry in std::fs::read_dir(root.join(".partial"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "part") {
            continue;
        }
        let sha = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let size = entry.metadata()?.len();
        total = total.saturating_add(size);
        if live.contains(&sha) {
            continue;
        }
        let meta = path.with_extension("meta");
        let progress = std::fs::read(&meta)
            .ok()
            .and_then(|b| serde_json::from_slice::<Meta>(&b).ok())
            .map_or(0, |m| m.progress);
        partials.push((progress, size, path, meta));
    }
    partials.sort_by_key(|p| p.0);
    for (progress, size, path, meta) in partials {
        if now().saturating_sub(progress) > Duration::from_secs(7 * 24 * 60 * 60).as_secs()
            || total > 2 * 1024 * 1024 * 1024
        {
            std::fs::remove_file(path)?;
            let _ = std::fs::remove_file(meta);
            total = total.saturating_sub(size);
        }
    }
    Ok(())
}
