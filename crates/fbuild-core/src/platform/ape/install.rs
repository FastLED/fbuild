//! Owner-only, atomic installation of executables fbuild derives from APE
//! images (extracted loaders, `ape` PATH entries, launch shims).
//!
//! A candidate directory is used only when it is a real directory owned by
//! the effective user with no group/other write access (so no other account
//! can plant or swap a file in it) and sits on a mount that allows exec.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::super::{fs, process};

/// Install `bytes` as executable `<dir>/<subdir>/<name>` in the first usable
/// candidate `dir`, reusing an identical existing file. `None` when no
/// candidate is usable.
pub(crate) fn install(
    dirs: &[PathBuf],
    subdir: Option<&str>,
    name: &str,
    bytes: &[u8],
) -> Option<PathBuf> {
    dirs.iter().find_map(|dir| {
        let dir = match subdir {
            Some(sub) => {
                // The parent must pass the same checks before we nest in it.
                if !usable_dir(dir) {
                    return None;
                }
                dir.join(sub)
            }
            None => dir.clone(),
        };
        install_in(&dir, bytes, name)
    })
}

fn usable_dir(dir: &Path) -> bool {
    fs::ensure_private_dir(dir) && fs::mount_allows_exec(dir)
}

pub(crate) fn install_in(dir: &Path, bytes: &[u8], name: &str) -> Option<PathBuf> {
    if !usable_dir(dir) {
        return None;
    }
    let target = dir.join(name);
    if is_installed(&target, bytes) {
        return Some(target);
    }
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let tmp = dir.join(format!(
        ".{name}.{}.{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    // No child may inherit the writable fd, or exec of the file would hit
    // ETXTBSY until that child execs (see `process::exclusive_fork_guard`).
    let written = {
        let _fork = process::exclusive_fork_guard();
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .and_then(|mut file| {
                file.write_all(bytes)?;
                file.sync_all()
            })
    }
    .and_then(|()| fs::set_executable(&tmp))
    // rename(2) is atomic: concurrent installers race benignly to identical
    // content, and readers never see a partial file.
    .and_then(|()| std::fs::rename(&tmp, &target));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return None;
    }
    is_installed(&target, bytes).then_some(target)
}

/// A regular (not symlinked), executable file whose content is exactly `bytes`.
pub(crate) fn is_installed(path: &Path, bytes: &[u8]) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| {
        meta.file_type().is_file() && fs::is_executable(&meta) && meta.len() == bytes.len() as u64
    }) && std::fs::read(path).is_ok_and(|content| content == bytes)
}
