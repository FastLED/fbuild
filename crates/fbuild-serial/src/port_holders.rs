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

/// Scan a `/proc`-shaped tree rooted at `proc_root` for processes holding
/// `port` open. Split out so tests can use a synthetic tree.
pub fn find_port_holders_in(proc_root: &Path, port: &str) -> Vec<PortHolder> {
    let target = std::fs::canonicalize(port).unwrap_or_else(|_| PathBuf::from(port));
    let mut holders = Vec::new();
    let Ok(entries) = std::fs::read_dir(proc_root) else {
        return holders;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else {
            continue;
        };
        let holds = fds
            .flatten()
            .filter_map(|fd| std::fs::read_link(fd.path()).ok())
            .any(|link| link == target || link == Path::new(port));
        if holds {
            let name = std::fs::read_to_string(entry.path().join("comm"))
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| "unknown".to_string());
            holders.push(PortHolder { pid, name });
        }
    }
    holders.sort_by_key(|h| h.pid);
    holders
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

    #[cfg(unix)]
    #[test]
    fn finds_holder_in_synthetic_proc_tree() {
        let dir = tempfile::TempDir::new().unwrap();
        let node = dir.path().join("ttyFAKE");
        std::fs::write(&node, b"").unwrap();
        let node = std::fs::canonicalize(&node).unwrap();

        let proc_root = dir.path().join("proc");
        for (pid, comm, holds) in [(10, "other", false), (42, "fbuild-daemon", true)] {
            let fd_dir = proc_root.join(pid.to_string()).join("fd");
            std::fs::create_dir_all(&fd_dir).unwrap();
            std::fs::write(
                proc_root.join(pid.to_string()).join("comm"),
                format!("{comm}\n"),
            )
            .unwrap();
            if holds {
                std::os::unix::fs::symlink(&node, fd_dir.join("3")).unwrap();
            }
        }

        let holders = find_port_holders_in(&proc_root, node.to_str().unwrap());
        assert_eq!(
            holders,
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
