//! Identify which process holds a serial port open (FastLED/fbuild#1429).
//!
//! An `open()` that fails with `EBUSY` was previously reported as
//! "serial driver may be wedged", which sent investigations to the device
//! and USB layer when the port was simply held by another fd (often the
//! daemon itself). This scans `/proc/*/fd` so the error can name the holder.
//! On hosts without `/proc` the scan finds nothing and callers fall back to
//! the generic message.

use std::path::Path;

use fbuild_core::path::{NormalizedPath, canonicalize_existing};

/// A process that has the port's device node open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortHolder {
    pub pid: u32,
    pub name: String,
}

/// Pure matcher: `procs` yields `(pid, name, fd link targets)`; returns the
/// processes with an fd pointing at `port` or at its resolved `canonical`
/// path.
pub fn match_port_holders(
    procs: impl IntoIterator<Item = (u32, String, Vec<NormalizedPath>)>,
    port: &NormalizedPath,
    canonical: &NormalizedPath,
) -> Vec<PortHolder> {
    let mut holders: Vec<PortHolder> = procs
        .into_iter()
        .filter(|(_, _, links)| links.iter().any(|link| link == canonical || link == port))
        .map(|(pid, name, _)| PortHolder { pid, name })
        .collect();
    holders.sort_by_key(|h| h.pid);
    holders
}

/// Read a `/proc`-shaped tree rooted at `proc_root` into `(pid, comm, fds)`.
fn read_proc_tree(proc_root: &Path) -> Vec<(u32, String, Vec<NormalizedPath>)> {
    let Ok(entries) = std::fs::read_dir(proc_root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse::<u32>().ok()?;
            let fds = std::fs::read_dir(entry.path().join("fd")).ok()?;
            let links = fds
                .flatten()
                .filter_map(|fd| std::fs::read_link(fd.path()).ok())
                .map(NormalizedPath::from)
                .collect();
            let name = std::fs::read_to_string(entry.path().join("comm"))
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| "unknown".to_string());
            Some((pid, name, links))
        })
        .collect()
}

/// Longest the diagnostic may delay an error response.
const SCAN_BUDGET: std::time::Duration = std::time::Duration::from_millis(500);

/// Scan a `/proc`-shaped tree for processes holding `port` open. The scan
/// walks every process's fd table, so it runs on the blocking pool with a
/// bounded wait; `None` means the scan did not finish in time.
pub async fn find_port_holders_in(
    proc_root: &NormalizedPath,
    port: &str,
) -> Option<Vec<PortHolder>> {
    let port_path = NormalizedPath::new(port);
    let canonical = canonicalize_existing(port)
        .await
        .unwrap_or_else(|_| port_path.clone());
    let root = proc_root.clone();
    let scan = tokio::task::spawn_blocking(move || read_proc_tree(root.as_path()));
    let procs = tokio::time::timeout(SCAN_BUDGET, scan).await.ok()?.ok()?;
    Some(match_port_holders(procs, &port_path, &canonical))
}

/// Processes currently holding `port` open (Linux `/proc`; empty elsewhere).
pub async fn find_port_holders(port: &str) -> Vec<PortHolder> {
    find_port_holders_in(&NormalizedPath::new("/proc"), port)
        .await
        .unwrap_or_default()
}

/// Human-readable contention hint, e.g. `port held by fbuild-daemon (pid 42)`.
/// `None` when no holder is visible.
pub async fn describe_port_holders(port: &str) -> Option<String> {
    let holders = find_port_holders(port).await;
    if holders.is_empty() {
        return None;
    }
    let list = holders
        .iter()
        .map(|h| format!("{} (pid {})", h.name, h.pid))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!("port held by {list}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_only_processes_with_an_fd_on_the_port() {
        let procs = vec![
            (
                10,
                "other".to_string(),
                vec![NormalizedPath::new("/dev/null")],
            ),
            (
                42,
                "fbuild-daemon".to_string(),
                vec![NormalizedPath::new("/dev/ttyFAKE")],
            ),
        ];
        assert_eq!(
            match_port_holders(
                procs,
                &NormalizedPath::new("/dev/ttyFAKE"),
                &NormalizedPath::new("/dev/ttyFAKE")
            ),
            vec![PortHolder {
                pid: 42,
                name: "fbuild-daemon".to_string()
            }]
        );
    }

    #[tokio::test]
    async fn missing_proc_root_yields_no_holders() {
        let dir = tempfile::TempDir::new().unwrap();
        let missing = NormalizedPath::new(dir.path().join("nope"));
        assert_eq!(
            find_port_holders_in(&missing, "/dev/ttyX").await,
            Some(Vec::new())
        );
    }
}
