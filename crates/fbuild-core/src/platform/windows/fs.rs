use std::fs::OpenOptions;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use crate::platform::fs::{ErrorClass, VolumeFacts, VolumeKind};
use crate::path::NormalizedPath;

pub(crate) fn file_identity(path: &Path) -> std::io::Result<same_file::Handle> {
    same_file::Handle::from_path(path)
}

pub(crate) fn comparison_key(path: &Path) -> String {
    let mut value = display_slash(path);
    if let Some(stripped) = value.strip_prefix("//?/") {
        value = stripped.to_string();
    }
    value.make_ascii_lowercase();
    value
}

pub(crate) fn display_slash(path: &Path) -> String {
    let mut value = path.to_string_lossy().replace('\\', "/");
    if let Some(stripped) = value.strip_prefix("//?/") {
        value = stripped.to_string();
    }
    value
}

pub(crate) fn strip_extended_prefix(path: &Path) -> Box<Path> {
    let value = path.to_string_lossy();
    if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
        let stripped = format!(r"\\{rest}");
        return Path::new(&stripped).into();
    }
    value
        .strip_prefix(r"\\?\")
        .map_or_else(|| Path::new(value.as_ref()).into(), |rest| Path::new(rest).into())
}

pub(crate) fn set_executable(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

pub(crate) fn ensure_executable(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

pub(crate) fn symlink_dir(original: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(original, link)
}

pub(crate) fn is_link_or_reparse(path: &Path) -> std::io::Result<bool> {
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    Ok(std::fs::symlink_metadata(path)?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
}

pub(crate) fn open_shared_write(
    options: &mut OpenOptions,
    path: &Path,
) -> std::io::Result<std::fs::File> {
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    options
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(path)
}

pub(crate) fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}

pub(crate) fn volume_facts(path: &Path) -> std::io::Result<VolumeFacts> {
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetVolumeInformationW, GetVolumePathNameW};

    const DRIVE_REMOVABLE: u32 = 2;
    const FILE_READ_ONLY_VOLUME: u32 = 0x0008_0000;

    let absolute = std::path::absolute(path)?;
    let wide_path = absolute
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut root = vec![0_u16; 32_768];
    // SAFETY: both UTF-16 buffers remain live for the call; `root` advertises
    // its full writable capacity and `wide_path` is NUL-terminated.
    if unsafe { GetVolumePathNameW(wide_path.as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    let root_len = root.iter().position(|unit| *unit == 0).unwrap_or(root.len() - 1);
    root.truncate(root_len + 1);

    let mut flags = 0_u32;
    // SAFETY: `root` is NUL-terminated. Optional output buffers are null and
    // their corresponding lengths are zero; `flags` is writable.
    if unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut flags,
            std::ptr::null_mut(),
            0,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }

    // SAFETY: `root` is the NUL-terminated volume root returned above.
    let kind = if unsafe { GetDriveTypeW(root.as_ptr()) } == DRIVE_REMOVABLE {
        VolumeKind::Removable
    } else {
        VolumeKind::Other
    };
    Ok(VolumeFacts {
        kind,
        total_space: fs2::total_space(path)?,
        available_space: fs2::available_space(path)?,
        read_only: flags & FILE_READ_ONLY_VOLUME != 0,
    })
}

pub(crate) fn removable_volume_roots() -> std::io::Result<Vec<NormalizedPath>> {
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};

    // SAFETY: GetLogicalDrives has no pointer arguments or preconditions.
    let mask = unsafe { GetLogicalDrives() };
    if mask == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(removable_roots_from_mask(mask, |letter| {
        let root = [u16::from(letter), u16::from(b':'), u16::from(b'\\'), 0];
        // SAFETY: `root` is a live, NUL-terminated drive-root buffer.
        unsafe { GetDriveTypeW(root.as_ptr()) }
    }))
}

fn removable_roots_from_mask(
    mask: u32,
    mut drive_type: impl FnMut(u8) -> u32,
) -> Vec<NormalizedPath> {
    const DRIVE_REMOVABLE: u32 = 2;

    (0_u8..26)
        .filter(|index| mask & (1_u32 << index) != 0)
        .filter_map(|index| {
            let letter = b'A' + index;
            (drive_type(letter) == DRIVE_REMOVABLE)
                .then(|| NormalizedPath::new(format!("{}:\\", char::from(letter))))
        })
        .collect()
}

pub(crate) fn classify_error(error: &std::io::Error) -> ErrorClass {
    match error.raw_os_error() {
        Some(5) => ErrorClass::AccessDenied,
        Some(121) => ErrorClass::TimedOut,
        Some(1006) => ErrorClass::InvalidatedHandle,
        Some(1392) => ErrorClass::CorruptFilesystem,
        Some(2 | 3 | 6 | 21 | 1167) => ErrorClass::DeviceUnavailable,
        _ => crate::platform::fs::portable_error_class(error).unwrap_or(ErrorClass::Other),
    }
}

pub(crate) fn cancel_blocking_io(
    worker: &std::thread::JoinHandle<()>,
) -> std::io::Result<bool> {
    use windows_sys::Win32::System::IO::CancelSynchronousIo;

    // SAFETY: `as_raw_handle` is valid for the lifetime of `worker`; Windows
    // permits cancellation through another live thread's handle.
    Ok(unsafe { CancelSynchronousIo(worker.as_raw_handle() as isize) } != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_writer_denies_delete_access() {
        const DELETE_ACCESS: u32 = 0x0001_0000;
        const ERROR_SHARING_VIOLATION: i32 = 32;

        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("NEW.UF2");
        let _writer = crate::platform::fs::open_shared_write(&destination).unwrap();
        let error = OpenOptions::new()
            .access_mode(DELETE_ACCESS)
            .open(&destination)
            .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(ERROR_SHARING_VIOLATION));
    }

    #[test]
    fn removable_snapshot_classifies_only_present_drives_once() {
        let mut queried = Vec::new();
        let roots = removable_roots_from_mask((1 << 2) | (1 << 3), |letter| {
            queried.push(letter);
            if letter == b'D' { 2 } else { 3 }
        });
        assert_eq!(queried, vec![b'C', b'D']);
        assert_eq!(roots, vec![NormalizedPath::new(r"D:\")]);
    }
}
