use std::fs::OpenOptions;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};

use crate::path::NormalizedPath;
use crate::platform::fs::{ErrorClass, LongPathSupport, VolumeFacts, VolumeKind};

/// A handle to exactly the system filesystem policy key, never an arbitrary
/// registry location supplied by a caller.
struct FileSystemPolicyKey(windows_sys::Win32::System::Registry::HKEY);

impl FileSystemPolicyKey {
    fn open(access: u32) -> std::io::Result<Self> {
        use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RegOpenKeyExW};

        let path = r"SYSTEM\CurrentControlSet\Control\FileSystem"
            .encode_utf16()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let mut key = 0;
        // SAFETY: `path` is a live NUL-terminated UTF-16 buffer and `key` is
        // writable storage. This opens only the fixed HKLM filesystem key.
        let status =
            unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, path.as_ptr(), 0, access, &mut key) };
        if status != 0 {
            return Err(std::io::Error::from_raw_os_error(status as i32));
        }
        Ok(Self(key))
    }
}

impl Drop for FileSystemPolicyKey {
    fn drop(&mut self) {
        // SAFETY: the handle belongs to this object after a successful open,
        // and it is closed exactly once without escaping the selected module.
        unsafe { windows_sys::Win32::System::Registry::RegCloseKey(self.0) };
    }
}

fn long_path_value_name() -> Vec<u16> {
    "LongPathsEnabled".encode_utf16().chain(Some(0)).collect()
}

pub(crate) fn long_path_support() -> std::io::Result<LongPathSupport> {
    use windows_sys::Win32::System::Registry::{KEY_QUERY_VALUE, RegQueryValueExW};

    let key = match FileSystemPolicyKey::open(KEY_QUERY_VALUE) {
        Ok(key) => key,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LongPathSupport::Disabled);
        }
        Err(error) => return Err(error),
    };
    let name = long_path_value_name();
    let mut value = [0_u8; 4];
    let mut value_type = 0;
    let mut value_length = value.len() as u32;
    // SAFETY: the handle remains open; `name` is NUL-terminated; the writable
    // output pointers remain live and `value_length` advertises buffer size.
    let status = unsafe {
        RegQueryValueExW(
            key.0,
            name.as_ptr(),
            std::ptr::null(),
            &mut value_type,
            value.as_mut_ptr(),
            &mut value_length,
        )
    };
    decode_long_path_value(status, value_type, &value, value_length)
}

fn decode_long_path_value(
    status: u32,
    value_type: u32,
    value: &[u8],
    reported_length: u32,
) -> std::io::Result<LongPathSupport> {
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA};
    use windows_sys::Win32::System::Registry::REG_DWORD;

    if status == ERROR_FILE_NOT_FOUND {
        return Ok(LongPathSupport::Disabled);
    }
    if status == ERROR_MORE_DATA {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "LongPathsEnabled exceeds the size of a REG_DWORD",
        ));
    }
    if status != 0 {
        return Err(std::io::Error::from_raw_os_error(status as i32));
    }
    if value_type != REG_DWORD || reported_length != 4 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "LongPathsEnabled must be a four-byte REG_DWORD",
        ));
    }
    let value: [u8; 4] = value.try_into().map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "LongPathsEnabled returned incomplete DWORD data",
        )
    })?;
    Ok(if u32::from_le_bytes(value) == 1 {
        LongPathSupport::Enabled
    } else {
        LongPathSupport::Disabled
    })
}

pub(crate) fn enable_long_paths() -> std::io::Result<()> {
    use windows_sys::Win32::System::Registry::{KEY_SET_VALUE, REG_DWORD, RegSetValueExW};

    let key = FileSystemPolicyKey::open(KEY_SET_VALUE)?;
    let name = long_path_value_name();
    let value = 1_u32.to_le_bytes();
    // SAFETY: `name` is NUL-terminated; `value` is a live four-byte DWORD;
    // the handle permits setting values on only the fixed filesystem key.
    let status = unsafe {
        RegSetValueExW(
            key.0,
            name.as_ptr(),
            0,
            REG_DWORD,
            value.as_ptr(),
            value.len() as u32,
        )
    };
    if status != 0 {
        return Err(std::io::Error::from_raw_os_error(status as i32));
    }
    drop(key);
    verify_long_paths_enabled(long_path_support()?)
}

fn verify_long_paths_enabled(support: LongPathSupport) -> std::io::Result<()> {
    if support == LongPathSupport::Enabled {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "LongPathsEnabled was not enabled after repair",
        ))
    }
}

pub(crate) fn legacy_path_units(path: &Path) -> usize {
    path.as_os_str().encode_wide().count()
}

pub(crate) fn short_cache_base() -> std::io::Result<PathBuf> {
    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;

    let drive = std::env::var_os("SystemDrive").ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "SystemDrive is unavailable; cannot select a short Windows cache root",
        )
    })?;
    let base = short_cache_base_from_drive(&drive)?;
    let root = base
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    // SAFETY: `root` is a live NUL-terminated absolute drive-root buffer.
    let drive_type = unsafe { GetDriveTypeW(root.as_ptr()) };
    // Fixed drives (3) keep the automatic cache on a stable local volume.
    if drive_type != 3 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "SystemDrive does not refer to an available fixed local cache drive",
        ));
    }
    Ok(base)
}

fn short_cache_base_from_drive(drive: &std::ffi::OsStr) -> std::io::Result<PathBuf> {
    let value = drive.to_str().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "SystemDrive is not a local drive letter",
        )
    })?;
    let bytes = value.as_bytes();
    if !(bytes.len() == 2 || bytes.len() == 3 && matches!(bytes[2], b'\\' | b'/'))
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "SystemDrive must be a local drive root such as C:",
        ));
    }
    Ok(PathBuf::from(format!(
        "{}:\\",
        char::from(bytes[0].to_ascii_uppercase())
    )))
}

pub(crate) fn ensure_private_directory(path: &Path) -> std::io::Result<()> {
    super::private_directory::ensure_private_directory(path)
}

pub(crate) fn file_identity(path: &Path) -> std::io::Result<same_file::Handle> {
    same_file::Handle::from_path(path)
}

pub(crate) fn executable_memo_identity(metadata: &std::fs::Metadata) -> std::io::Result<String> {
    Ok(format!(
        "{} {} {} {}",
        metadata.file_size(),
        metadata.last_write_time(),
        metadata.creation_time(),
        metadata.file_attributes()
    ))
}

pub(crate) fn comparison_key(path: &Path) -> String {
    let mut value = display_slash(path);
    value.make_ascii_lowercase();
    value
}

pub(crate) fn display_slash(path: &Path) -> String {
    let value = path.to_string_lossy().replace('\\', "/");
    if let Some(rest) = value.strip_prefix("//?/UNC/") {
        return format!("//{rest}");
    }
    value
        .strip_prefix("//?/")
        .map_or_else(|| value.clone(), str::to_string)
}

pub(crate) fn strip_extended_prefix(path: &Path) -> Box<Path> {
    let value = path.to_string_lossy();
    if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
        let stripped = format!(r"\\{rest}");
        return Path::new(&stripped).into();
    }
    value.strip_prefix(r"\\?\").map_or_else(
        || Path::new(value.as_ref()).into(),
        |rest| Path::new(rest).into(),
    )
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

pub(crate) fn symlink_file(original: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(original, link)
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

pub(crate) fn rename_path(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}

pub(crate) fn volume_facts(path: &Path) -> std::io::Result<VolumeFacts> {
    use windows_sys::Win32::Storage::FileSystem::{
        GetDriveTypeW, GetVolumeInformationW, GetVolumePathNameW,
    };

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
    let root_len = root
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(root.len() - 1);
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

pub(crate) fn cancel_blocking_io(worker: &std::thread::JoinHandle<()>) -> std::io::Result<bool> {
    use windows_sys::Win32::System::IO::CancelSynchronousIo;

    // SAFETY: `as_raw_handle` is valid for the lifetime of `worker`; Windows
    // permits cancellation through another live thread's handle.
    Ok(unsafe { CancelSynchronousIo(worker.as_raw_handle() as isize) } != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn native_long_path_registry_query_is_read_only() {
        let support = long_path_support().expect("read system filesystem policy");
        assert!(matches!(
            support,
            LongPathSupport::Enabled | LongPathSupport::Disabled
        ));
    }

    #[test]
    fn long_path_registry_missing_and_disabled_values_are_disabled() {
        use windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND;
        use windows_sys::Win32::System::Registry::REG_DWORD;

        assert_eq!(
            decode_long_path_value(ERROR_FILE_NOT_FOUND, 0, &[], 0).unwrap(),
            LongPathSupport::Disabled
        );
        for value in [0_u32, 2, u32::MAX] {
            assert_eq!(
                decode_long_path_value(0, REG_DWORD, &value.to_le_bytes(), 4).unwrap(),
                LongPathSupport::Disabled
            );
        }
        assert_eq!(
            decode_long_path_value(0, REG_DWORD, &1_u32.to_le_bytes(), 4).unwrap(),
            LongPathSupport::Enabled
        );
    }

    #[test]
    fn malformed_long_path_registry_values_report_invalid_data() {
        use windows_sys::Win32::Foundation::ERROR_MORE_DATA;
        use windows_sys::Win32::System::Registry::{REG_DWORD, REG_SZ};

        for (status, kind, bytes, length) in [
            (0, REG_SZ, &[1_u8, 0, 0, 0][..], 4),
            (0, REG_DWORD, &[1_u8, 0][..], 2),
            (0, REG_DWORD, &[1_u8, 0, 0, 0][..], 8),
            (ERROR_MORE_DATA, REG_DWORD, &[1_u8, 0, 0, 0][..], 8),
        ] {
            assert_eq!(
                decode_long_path_value(status, kind, bytes, length)
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::InvalidData
            );
        }
    }

    #[test]
    fn long_path_registry_read_errors_preserve_the_native_error() {
        use windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;

        let error = decode_long_path_value(ERROR_ACCESS_DENIED, 0, &[], 0).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(ERROR_ACCESS_DENIED as i32));
    }

    #[test]
    fn long_path_repair_requires_verified_enabled_state() {
        assert!(verify_long_paths_enabled(LongPathSupport::Enabled).is_ok());
        for support in [LongPathSupport::Disabled, LongPathSupport::NotApplicable] {
            assert_eq!(
                verify_long_paths_enabled(support).unwrap_err().kind(),
                std::io::ErrorKind::InvalidData
            );
        }
    }

    #[test]
    fn short_cache_base_accepts_only_a_local_drive_root() {
        use std::ffi::OsStr;

        assert_eq!(
            short_cache_base_from_drive(OsStr::new("C:")).unwrap(),
            Path::new(r"C:\")
        );
        assert_eq!(
            short_cache_base_from_drive(OsStr::new(r"D:\")).unwrap(),
            Path::new(r"D:\")
        );
        for drive in ["", "C", r"C:\nested", r"\\server\share", "1:", "C:relative"] {
            assert_eq!(
                short_cache_base_from_drive(OsStr::new(drive))
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::InvalidInput
            );
        }
    }

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

    #[test]
    fn extended_unc_paths_match_plain_unc_paths() {
        let extended = Path::new(r"\\?\UNC\server\share\file");
        let plain = Path::new(r"\\server\share\file");
        assert_eq!(display_slash(extended), "//server/share/file");
        assert_eq!(comparison_key(extended), comparison_key(plain));
    }
}
