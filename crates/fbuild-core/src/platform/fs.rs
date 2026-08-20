//! Neutral filesystem identity, permission, and replacement APIs.

use std::fs::{File, OpenOptions};
use std::path::Path;

use crate::path::NormalizedPath;

/// Opaque, open-handle identity for a filesystem object.
#[derive(Debug, Eq, PartialEq, Hash)]
pub struct FileIdentity(same_file::Handle);

/// Host classification of the volume containing a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeKind {
    Removable,
    Other,
}

/// Neutral facts about the volume containing a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeFacts {
    pub kind: VolumeKind,
    pub total_space: u64,
    pub available_space: u64,
    pub read_only: bool,
}

/// Stable classes for host-native filesystem errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    AccessDenied,
    TimedOut,
    InvalidatedHandle,
    CorruptFilesystem,
    DeviceUnavailable,
    Other,
}

/// Open a path identity that compares by underlying filesystem object.
pub fn file_identity(path: &Path) -> std::io::Result<FileIdentity> {
    super::selected::fs::file_identity(path).map(FileIdentity)
}

/// Return whether two paths resolve to the same underlying filesystem object.
pub fn same_file(left: &Path, right: &Path) -> std::io::Result<bool> {
    Ok(file_identity(left)? == file_identity(right)?)
}

/// Normalize a lexical path into the host's comparison-key representation.
#[must_use]
pub fn comparison_key(path: &Path) -> String {
    super::selected::fs::comparison_key(path)
}

/// Render a path using the stable, case-preserving slash form.
#[must_use]
pub fn display_slash(path: &Path) -> String {
    super::selected::fs::display_slash(path)
}

/// Remove host-added extended-length prefixes from a canonical path.
#[must_use]
pub fn strip_extended_prefix(path: &Path) -> Box<Path> {
    super::selected::fs::strip_extended_prefix(path)
}

/// Set executable permissions where the host represents them.
pub fn set_executable(path: &Path) -> std::io::Result<()> {
    super::selected::fs::set_executable(path)
}

/// Ensure an extracted tool is executable without changing an already-runnable file.
pub fn ensure_executable(path: &Path) -> std::io::Result<()> {
    super::selected::fs::ensure_executable(path)
}

/// Create a directory symlink using the host-native operation.
pub fn symlink_dir(original: &Path, link: &Path) -> std::io::Result<()> {
    super::selected::fs::symlink_dir(original, link)
}

/// Return whether a path is a symbolic link or Windows reparse point.
pub fn is_link_or_reparse(path: &Path) -> std::io::Result<bool> {
    super::selected::fs::is_link_or_reparse(path)
}

/// Open a create/truncate destination with removable-volume-safe sharing.
pub fn open_shared_write(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    super::selected::fs::open_shared_write(&mut options, path)
}

/// Atomically replace `destination` with `source` on the same filesystem.
pub fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    super::selected::fs::replace_file(source, destination)
}

/// Atomically rename a file or directory on the same filesystem.
pub fn rename_path(source: &Path, destination: &Path) -> std::io::Result<()> {
    super::selected::fs::replace_file(source, destination)
}

/// Async bridge for [`replace_file`], dispatched away from the Tokio worker.
pub async fn replace_file_async(source: &Path, destination: &Path) -> std::io::Result<()> {
    let source = source.to_path_buf();
    let destination = destination.to_path_buf();
    tokio::task::spawn_blocking(move || replace_file(&source, &destination))
        .await
        .map_err(std::io::Error::other)?
}

/// Total capacity of the filesystem containing `path`.
pub fn total_space(path: &Path) -> std::io::Result<u64> {
    Ok(volume_facts(path)?.total_space)
}

/// Query neutral capacity, writability, and host volume-kind facts.
pub fn volume_facts(path: &Path) -> std::io::Result<VolumeFacts> {
    super::selected::fs::volume_facts(path)
}

/// Snapshot mounted removable volume roots without probing candidate paths.
pub fn removable_volume_roots() -> std::io::Result<Vec<NormalizedPath>> {
    super::selected::fs::removable_volume_roots()
}

/// Classify a native filesystem error without exposing host error numbers.
#[must_use]
pub fn classify_error(error: &std::io::Error) -> ErrorClass {
    super::selected::fs::classify_error(error)
}

pub(crate) fn portable_error_class(error: &std::io::Error) -> Option<ErrorClass> {
    use std::io::ErrorKind;

    match error.kind() {
        ErrorKind::NotFound
        | ErrorKind::BrokenPipe
        | ErrorKind::ConnectionAborted
        | ErrorKind::ConnectionReset
        | ErrorKind::UnexpectedEof => Some(ErrorClass::DeviceUnavailable),
        _ => None,
    }
}

/// Ask the host to retire blocking filesystem I/O owned by `worker`.
pub fn cancel_blocking_io(worker: &std::thread::JoinHandle<()>) -> std::io::Result<bool> {
    super::selected::fs::cancel_blocking_io(worker)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_path_rules_match_the_current_host() {
        let mixed = Path::new(r"Case\Mixed");
        let key = comparison_key(mixed);
        if crate::platform::host::is_windows() {
            assert_eq!(key, "case/mixed");
            assert_eq!(display_slash(mixed), "Case/Mixed");
        } else if crate::platform::host::current().os() == crate::platform::host::HostOs::Macos {
            assert_eq!(key, r"case\mixed");
        } else {
            assert_eq!(key, r"Case\Mixed");
        }
    }

    #[test]
    fn replacement_overwrites_an_existing_destination() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        std::fs::write(&source, b"new").expect("write source");
        std::fs::write(&destination, b"old").expect("write destination");
        replace_file(&source, &destination).expect("replace destination");
        assert_eq!(std::fs::read(&destination).unwrap(), b"new");
        assert!(!source.exists());
    }

    #[test]
    fn identities_distinguish_paths_and_match_aliases() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = temp.path().join("first");
        let alias = temp.path().join("alias");
        let second = temp.path().join("second");
        std::fs::write(&first, b"first").unwrap();
        std::fs::hard_link(&first, &alias).unwrap();
        std::fs::write(&second, b"second").unwrap();
        assert!(same_file(&first, &alias).unwrap());
        assert!(!same_file(&first, &second).unwrap());
    }

    #[test]
    fn volume_facts_are_internally_consistent() {
        let temp = tempfile::tempdir().expect("tempdir");
        let nested = temp.path().join("read-only-file");
        std::fs::write(&nested, b"contents").unwrap();
        let root_facts = volume_facts(temp.path()).unwrap();
        let mut permissions = std::fs::metadata(&nested).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&nested, permissions).unwrap();
        let facts = volume_facts(&nested).unwrap();
        assert!(facts.total_space > 0);
        assert!(facts.available_space <= facts.total_space);
        assert_eq!(facts.read_only, root_facts.read_only);
    }

    #[test]
    fn windows_error_numbers_are_not_reinterpreted_on_unix() {
        let input_output = std::io::Error::from_raw_os_error(5);
        let remote_input_output = std::io::Error::from_raw_os_error(121);
        if crate::platform::host::is_windows() {
            assert_eq!(classify_error(&input_output), ErrorClass::AccessDenied);
            assert_eq!(classify_error(&remote_input_output), ErrorClass::TimedOut);
        } else {
            assert_ne!(classify_error(&input_output), ErrorClass::AccessDenied);
            assert_ne!(classify_error(&remote_input_output), ErrorClass::TimedOut);
        }
    }

    #[test]
    fn linux_tmpfs_capacity_is_queried_for_the_exact_mount_when_available() {
        if crate::platform::host::current().os() != crate::platform::host::HostOs::Linux {
            return;
        }
        let tmpfs = Path::new("/dev/shm");
        if !tmpfs.exists() {
            return;
        }
        let facts = volume_facts(tmpfs).unwrap();
        assert_eq!(facts.total_space, fs2::total_space(tmpfs).unwrap());
        assert_eq!(facts.available_space, fs2::available_space(tmpfs).unwrap());
    }
}
