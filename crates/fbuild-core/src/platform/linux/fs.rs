use std::fs::OpenOptions;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::platform::fs::{ErrorClass, VolumeFacts};
use crate::path::NormalizedPath;

pub(crate) fn file_identity(path: &Path) -> std::io::Result<same_file::Handle> {
    same_file::Handle::from_path(path)
}

pub(crate) fn comparison_key(path: &Path) -> String {
    path.as_os_str().to_string_lossy().into_owned()
}

pub(crate) fn display_slash(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub(crate) fn strip_extended_prefix(path: &Path) -> Box<Path> {
    path.into()
}

pub(crate) fn set_executable(path: &Path) -> std::io::Result<()> {
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(permissions.mode() | 0o755);
    std::fs::set_permissions(path, permissions)
}

pub(crate) fn ensure_executable(path: &Path) -> std::io::Result<()> {
    let permissions = std::fs::metadata(path)?.permissions();
    if permissions.mode() & 0o111 == 0 {
        set_executable(path)?;
    }
    Ok(())
}

pub(crate) fn symlink_dir(original: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(original, link)
}

pub(crate) fn is_link_or_reparse(path: &Path) -> std::io::Result<bool> {
    Ok(std::fs::symlink_metadata(path)?.file_type().is_symlink())
}

pub(crate) fn open_shared_write(
    options: &mut OpenOptions,
    path: &Path,
) -> std::io::Result<std::fs::File> {
    options.open(path)
}

pub(crate) fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}

pub(crate) fn volume_facts(path: &Path) -> std::io::Result<VolumeFacts> {
    volume_facts_from_statvfs(path)
}

pub(crate) fn removable_volume_roots() -> std::io::Result<Vec<NormalizedPath>> {
    Ok(Vec::new())
}

pub(crate) fn classify_error(error: &std::io::Error) -> ErrorClass {
    match error.raw_os_error() {
        Some(19) => ErrorClass::DeviceUnavailable,
        _ => crate::platform::fs::portable_error_class(error).unwrap_or(ErrorClass::Other),
    }
}

pub(crate) fn cancel_blocking_io(
    _worker: &std::thread::JoinHandle<()>,
) -> std::io::Result<bool> {
    Ok(false)
}

fn volume_facts_from_statvfs(path: &Path) -> std::io::Result<VolumeFacts> {
    use std::os::unix::ffi::OsStrExt;

    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path contains a NUL byte")
    })?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is NUL-terminated and `stats` points to writable storage.
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: successful statvfs initialized the complete output structure.
    let stats = unsafe { stats.assume_init() };
    let fragment_size = stats.f_frsize as u128;
    Ok(VolumeFacts {
        kind: crate::platform::fs::VolumeKind::Other,
        total_space: byte_count(stats.f_blocks as u128, fragment_size),
        available_space: byte_count(stats.f_bavail as u128, fragment_size),
        read_only: stats.f_flag & libc::ST_RDONLY != 0,
    })
}

fn byte_count(blocks: u128, fragment_size: u128) -> u64 {
    blocks.saturating_mul(fragment_size).min(u128::from(u64::MAX)) as u64
}
