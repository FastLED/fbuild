//! Identify which process holds a serial port open (FastLED/fbuild#1429).
//!
//! An `open()` that fails with `EBUSY` was previously reported as
//! "serial driver may be wedged", which sent investigations to the device
//! and USB layer when the port was simply held by another fd (often the
//! daemon itself). This scans `/proc/*/fd` so the error can name the holder.
//! On hosts without `/proc` the scan finds nothing and callers fall back to
//! the generic message.

use std::path::{Path, PathBuf};

/// A process that has the port's device node open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortHolder {
    pub pid: u32,
    pub name: String,
}

/// Pure matcher: `procs` yields `(pid, name, fd link targets)`; returns the
/// processes with an fd pointing at `port` (or its canonical path).
pub fn match_port_holders(
    procs: impl IntoIterator<Item = (u32, String, Vec<PathBuf>)>,
    port: &str,
) -> Vec<PortHolder> {
    let target = std::fs::canonicalize(port).unwrap_or_else(|_| PathBuf::from(port));
    let mut holders: Vec<PortHolder> = procs
        .into_iter()
        .filter(|(_, _, links)| {
            links
                .iter()
                .any(|link| *link == target || link == Path::new(port))
        })
        .map(|(pid, name, _)| PortHolder { pid, name })
        .collect();
    holders.sort_by_key(|h| h.pid);
    holders
}

/// Read a `/proc`-shaped tree rooted at `proc_root` into `(pid, comm, fds)`.
fn read_proc_tree(proc_root: &Path) -> Vec<(u32, String, Vec<PathBuf>)> {
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
                .collect();
            let name = std::fs::read_to_string(entry.path().join("comm"))
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| "unknown".to_string());
            Some((pid, name, links))
        })
        .collect()
}

/// Scan a `/proc`-shaped tree for processes holding `port` open.
pub fn find_port_holders_in(proc_root: &Path, port: &str) -> Vec<PortHolder> {
    match_port_holders(read_proc_tree(proc_root), port)
}

/// Processes currently holding `port` open (Linux `/proc`; empty elsewhere).
pub fn find_port_holders(port: &str) -> Vec<PortHolder> {
    find_port_holders_in(Path::new("/proc"), port)
}

/// Human-readable contention hint, e.g. `port held by fbuild-daemon (pid 42)`.
/// `None` when no holder is visible.
pub fn describe_port_holders(port: &str) -> Option<String> {
    let holders = find_port_holders(port);
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
            (10, "other".to_string(), vec![PathBuf::from("/dev/null")]),
            (
                42,
                "fbuild-daemon".to_string(),
                vec![PathBuf::from("/dev/ttyFAKE")],
            ),
        ];
        assert_eq!(
            match_port_holders(procs, "/dev/ttyFAKE"),
            vec![PortHolder {
                pid: 42,
                name: "fbuild-daemon".to_string()
            }]
        );
    }

    #[test]
    fn missing_proc_root_yields_no_holders() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(find_port_holders_in(&dir.path().join("nope"), "/dev/ttyX").is_empty());
    }
}
