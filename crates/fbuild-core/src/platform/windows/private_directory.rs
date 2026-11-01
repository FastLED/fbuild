//! Native ownership and protected permissions for automatic cache directories.

use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use crate::path::NormalizedPath;

struct NativeHandle(windows_sys::Win32::Foundation::HANDLE);

impl Drop for NativeHandle {
    fn drop(&mut self) {
        // SAFETY: this object owns a successfully opened native handle.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

struct LocalAllocation(*mut std::ffi::c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: security APIs return LocalAlloc-owned storage; null is
        // accepted by LocalFree and successful allocations are freed once.
        unsafe { windows_sys::Win32::Foundation::LocalFree(self.0) };
    }
}

struct PrivateDirectorySecurity {
    // Keeps the current-user SID pointer live through every owner comparison.
    _token_user: Vec<usize>,
    sid: windows_sys::Win32::Security::PSID,
    descriptor: LocalAllocation,
    acl: *mut windows_sys::Win32::Security::ACL,
}

fn current_token_user() -> std::io::Result<Vec<usize>> {
    use windows_sys::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TokenUser};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token = std::ptr::null_mut();
    // SAFETY: current-process pseudo-handle is valid and `token` is writable.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let token = NativeHandle(token);
    let mut length = 0;
    // SAFETY: null plus zero length is the documented size query.
    let result =
        unsafe { GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut length) };
    if result != 0
        || std::io::Error::last_os_error().raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32)
    {
        return Err(std::io::Error::last_os_error());
    }
    if (length as usize) < std::mem::size_of::<windows_sys::Win32::Security::TOKEN_USER>() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Windows returned an incomplete token user",
        ));
    }
    // usize storage supplies TOKEN_USER's native pointer alignment, including
    // the SID bytes appended by GetTokenInformation.
    let mut user = vec![0_usize; (length as usize).div_ceil(std::mem::size_of::<usize>())];
    // SAFETY: aligned storage advertises at least `length` writable bytes.
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            user.as_mut_ptr().cast(),
            length,
            &mut length,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(user)
}

fn private_directory_security() -> std::io::Result<PrivateDirectorySecurity> {
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{GetSecurityDescriptorDacl, TOKEN_USER};

    let user = current_token_user()?;
    // SAFETY: current_token_user returned an aligned, initialized TOKEN_USER.
    let sid = unsafe { (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let mut sid_text = std::ptr::null_mut();
    // SAFETY: SID remains live in `user`; output points to owned allocation.
    if unsafe { ConvertSidToStringSidW(sid, &mut sid_text) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let allocated_sid = LocalAllocation(sid_text.cast());
    // SAFETY: ConvertSidToStringSidW returns a NUL-terminated UTF-16 string
    // that remains live until `allocated_sid` is dropped.
    let sid_text = unsafe { widestring::U16CStr::from_ptr_str(sid_text) }
        .to_string()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let descriptor_text = format!("O:{sid_text}D:P(A;OICI;FA;;;{sid_text})")
        .encode_utf16()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut descriptor = std::ptr::null_mut();
    // SAFETY: descriptor text is NUL-terminated and output is writable.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            descriptor_text.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    let descriptor = LocalAllocation(descriptor);
    let mut present = 0;
    let mut defaulted = 0;
    let mut acl = std::ptr::null_mut();
    // SAFETY: initialized descriptor is live; all output pointers are writable.
    if unsafe { GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut acl, &mut defaulted) }
        == 0
        || present == 0
        || acl.is_null()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "private directory security descriptor has no DACL",
        ));
    }
    drop(allocated_sid);
    Ok(PrivateDirectorySecurity {
        _token_user: user,
        sid,
        descriptor,
        acl,
    })
}

fn private_directory_components(path: &Path) -> std::io::Result<Vec<NormalizedPath>> {
    use std::path::{Component, Prefix};

    let invalid = || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "automatic private cache must be a local drive child or its descendants",
        )
    };
    if path.as_os_str().encode_wide().any(|unit| unit == 0) {
        return Err(invalid());
    }
    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return Err(invalid());
    };
    let Prefix::Disk(drive) = prefix.kind() else {
        return Err(invalid());
    };
    if !matches!(components.next(), Some(Component::RootDir)) {
        return Err(invalid());
    }
    let mut current = NormalizedPath::new(format!("{}:\\", char::from(drive)));
    let mut paths = vec![current.clone()];
    for component in components {
        let Component::Normal(name) = component else {
            return Err(invalid());
        };
        current = current.join(Path::new(name));
        paths.push(current.clone());
    }
    if paths.len() < 2 {
        return Err(invalid());
    }
    Ok(paths)
}

fn open_private_directory(path: &Path, writable_dacl: bool) -> std::io::Result<NativeHandle> {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING, READ_CONTROL,
    };

    let path = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    // SetSecurityInfo documents that MAXIMUM_ALLOWED handles suppress ACE
    // propagation to existing children. Secure each ownership-verified path
    // individually so an untargeted/foreign child is never changed as a side
    // effect of protecting its parent. Current owners receive WRITE_DAC.
    // https://learn.microsoft.com/en-us/windows/win32/api/aclapi/nf-aclapi-setsecurityinfo
    const MAXIMUM_ALLOWED: u32 = 0x0200_0000;
    let access = if writable_dacl {
        MAXIMUM_ALLOWED
    } else {
        READ_CONTROL
    };
    // SAFETY: path is NUL-terminated. No delete sharing keeps this directory
    // from being replaced while child traversal and ACL verification run.
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    let handle = NativeHandle(handle);
    let mut info = std::mem::MaybeUninit::uninit();
    // SAFETY: handle is live and info points to writable native output storage.
    if unsafe {
        windows_sys::Win32::Storage::FileSystem::GetFileInformationByHandle(
            handle.0,
            info.as_mut_ptr(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: successful GetFileInformationByHandle initialized `info`.
    let info = unsafe { info.assume_init() };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    };
    if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
        || info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "private cache directory must not be a file, junction, or reparse point",
        ));
    }
    Ok(handle)
}

fn verify_private_directory_owner(
    handle: &NativeHandle,
    security: &PrivateDirectorySecurity,
) -> std::io::Result<()> {
    use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{EqualSid, OWNER_SECURITY_INFORMATION};

    let mut owner = std::ptr::null_mut();
    let mut descriptor = std::ptr::null_mut();
    // SAFETY: handle remains open; all non-null output pointers are writable.
    let status = unsafe {
        GetSecurityInfo(
            handle.0,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    let descriptor = LocalAllocation(descriptor);
    if status != 0 {
        return Err(std::io::Error::from_raw_os_error(status as i32));
    }
    // SAFETY: descriptor keeps owner live and security keeps the token SID live.
    if owner.is_null() || unsafe { EqualSid(owner, security.sid) } == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "automatic cache directory belongs to another user; choose another short root",
        ));
    }
    drop(descriptor);
    Ok(())
}

fn protect_private_directory(
    handle: &NativeHandle,
    security: &PrivateDirectorySecurity,
) -> std::io::Result<()> {
    use windows_sys::Win32::Security::Authorization::{
        GetSecurityInfo, SE_FILE_OBJECT, SetSecurityInfo,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, GetSecurityDescriptorControl,
        PROTECTED_DACL_SECURITY_INFORMATION, SE_DACL_PROTECTED,
    };

    verify_private_directory_owner(handle, security)?;
    // SAFETY: only the verified current-user-owned directory's DACL changes;
    // the descriptor and ACL stay live and ownership is never assigned here.
    let status = unsafe {
        SetSecurityInfo(
            handle.0,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            security.acl,
            std::ptr::null_mut(),
        )
    };
    if status != 0 {
        return Err(std::io::Error::from_raw_os_error(status as i32));
    }
    let mut acl = std::ptr::null_mut();
    let mut descriptor = std::ptr::null_mut();
    // SAFETY: handle is live and output pointers are writable.
    let status = unsafe {
        GetSecurityInfo(
            handle.0,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut acl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    let descriptor = LocalAllocation(descriptor);
    if status != 0 {
        return Err(std::io::Error::from_raw_os_error(status as i32));
    }
    let mut control = 0;
    let mut revision = 0;
    // SAFETY: descriptor is initialized by the successful query above.
    if unsafe { GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision) } == 0
        || control & SE_DACL_PROTECTED == 0
        || acl.is_null()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "automatic cache DACL was not protected",
        ));
    }
    // SAFETY: Windows returned valid ACLs whose AclSize bounds initialized
    // storage. Exact comparison proves no additional users received access.
    let actual =
        unsafe { std::slice::from_raw_parts(acl.cast::<u8>(), usize::from((*acl).AclSize)) };
    let expected = unsafe {
        std::slice::from_raw_parts(
            security.acl.cast::<u8>(),
            usize::from((*security.acl).AclSize),
        )
    };
    if actual != expected {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "automatic cache DACL does not grant exclusive access to the current user",
        ));
    }
    Ok(())
}

pub(crate) fn ensure_private_directory(path: &Path) -> std::io::Result<()> {
    use windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        // A complete attempt owns its ancestor handles locally. Returning an
        // error releases every handle before this loop waits, so concurrent
        // initializers never hold competing partial hierarchies while retrying.
        match ensure_private_directory_once(path) {
            Err(error)
                if error.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32)
                    && std::time::Instant::now() < deadline =>
            {
                // SAFETY: this native delay has no preconditions. This
                // blocking capability runs on a dedicated blocking worker.
                unsafe { windows_sys::Win32::System::Threading::Sleep(20) };
            }
            result => return result,
        }
    }
}

fn ensure_private_directory_once(path: &Path) -> std::io::Result<()> {
    use windows_sys::Win32::Foundation::ERROR_ALREADY_EXISTS;
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::Storage::FileSystem::{CreateDirectoryW, GetDriveTypeW};

    let paths = private_directory_components(path)?;
    let root = paths[0]
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    // SAFETY: root is a live NUL-terminated local-drive root buffer.
    if unsafe { GetDriveTypeW(root.as_ptr()) } != 3 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "automatic cache root must be an available fixed local drive",
        ));
    }
    let security = private_directory_security()?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: security.descriptor.0,
        bInheritHandle: 0,
    };
    let mut handles = vec![open_private_directory(&paths[0], false)?];
    for directory in &paths[1..] {
        let wide = directory
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        // SAFETY: path/descriptor stay live. The protected owner-only DACL is
        // installed atomically with creation, before the directory is usable.
        if unsafe { CreateDirectoryW(wide.as_ptr(), &attributes) } == 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_ALREADY_EXISTS as i32) {
                return Err(error);
            }
        }
        let handle = open_private_directory(directory, true)?;
        protect_private_directory(&handle, &security)?;
        // Keep every ancestor locked against deletion/replacement until the
        // full hierarchy has been verified and protected.
        handles.push(handle);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::fs::short_cache_base;
    use super::*;

    /// Default Windows TempDir ownership can be TokenOwner (a group) rather
    /// than TokenUser. Create a uniquely named direct drive child with explicit
    /// current-user ownership through the API under test.
    struct PrivateFixture {
        path: NormalizedPath,
    }

    impl PrivateFixture {
        fn new() -> Self {
            use sha2::Digest;
            use std::sync::atomic::{AtomicU64, Ordering};

            static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
            let counter = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let nonce = format!("{}-{timestamp}-{counter}", std::process::id());
            let digest = sha2::Sha256::digest(nonce.as_bytes());
            let suffix = digest[..6]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            let path = short_cache_base()
                .unwrap()
                .join(format!("fb-private-{suffix}"));
            ensure_private_directory(&path).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            self.path.as_path()
        }
    }

    impl Drop for PrivateFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn private_directory_rejects_relative_unc_and_drive_roots() {
        for path in [r"relative", r"C:relative", r"\\server\share\fb", r"C:\"] {
            assert_eq!(
                ensure_private_directory(Path::new(path))
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn private_directory_is_repeatable_and_denies_anonymous_access() {
        use windows_sys::Win32::Security::{ImpersonateAnonymousToken, RevertToSelf};
        use windows_sys::Win32::System::Threading::GetCurrentThread;

        let temp = PrivateFixture::new();
        let path = temp.path().join("private");
        ensure_private_directory(&path).unwrap();
        ensure_private_directory(&path).unwrap();
        std::fs::write(path.join("owned"), b"private").unwrap();
        let denied_path = path.join("unauthorized");
        let error = std::thread::spawn(move || {
            // SAFETY: this test thread enters anonymous impersonation and
            // always reverts before returning to its caller.
            assert_ne!(unsafe { ImpersonateAnonymousToken(GetCurrentThread()) }, 0);
            let result = std::fs::write(denied_path, b"not allowed");
            assert_ne!(unsafe { RevertToSelf() }, 0);
            result.unwrap_err()
        })
        .join()
        .unwrap();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn private_directory_concurrent_initializers_finish_on_the_same_root() {
        let temp = PrivateFixture::new();
        let path = temp.path().join("shared").join("cache");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let workers = (0..4)
            .map(|_| {
                let barrier = std::sync::Arc::clone(&barrier);
                let path = path.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    ensure_private_directory(&path)
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().unwrap().unwrap();
        }
        std::fs::write(path.join("complete"), b"private").unwrap();
    }

    fn dacl_bytes(path: &Path) -> Vec<u8> {
        use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
        use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;

        let handle = open_private_directory(path, false).unwrap();
        let mut acl = std::ptr::null_mut();
        let mut descriptor = std::ptr::null_mut();
        // SAFETY: live directory handle and writable output pointers.
        assert_eq!(
            unsafe {
                GetSecurityInfo(
                    handle.0,
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut acl,
                    std::ptr::null_mut(),
                    &mut descriptor,
                )
            },
            0
        );
        let _descriptor = LocalAllocation(descriptor);
        assert!(!acl.is_null());
        // SAFETY: initialized ACL lives in the owned descriptor allocation.
        unsafe { std::slice::from_raw_parts(acl.cast::<u8>(), usize::from((*acl).AclSize)) }
            .to_vec()
    }

    #[test]
    fn private_directory_owner_mismatch_preserves_existing_permissions() {
        use windows_sys::Win32::Security::{CreateWellKnownSid, WinAnonymousSid};

        let temp = PrivateFixture::new();
        let before = dacl_bytes(temp.path());
        let mut security = private_directory_security().unwrap();
        let mut anonymous = [0_u64; 16];
        let mut length = std::mem::size_of_val(&anonymous) as u32;
        // SAFETY: aligned storage is large enough for a well-known SID.
        assert_ne!(
            unsafe {
                CreateWellKnownSid(
                    WinAnonymousSid,
                    std::ptr::null_mut(),
                    anonymous.as_mut_ptr().cast(),
                    &mut length,
                )
            },
            0
        );
        // Compare the real existing directory owner against another identity
        // without assigning ownership or requiring administrator privileges.
        security.sid = anonymous.as_mut_ptr().cast();
        let handle = open_private_directory(temp.path(), true).unwrap();
        let error = protect_private_directory(&handle, &security).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        drop(handle);
        assert_eq!(dacl_bytes(temp.path()), before);
    }

    #[test]
    fn private_directory_hardening_preserves_untargeted_child_permissions() {
        use windows_sys::Win32::Security::Authorization::{SE_FILE_OBJECT, SetSecurityInfo};
        use windows_sys::Win32::Security::{
            ACCESS_ALLOWED_ACE, DACL_SECURITY_INFORMATION, GetAce,
            PROTECTED_DACL_SECURITY_INFORMATION,
        };
        use windows_sys::Win32::Storage::FileSystem::DELETE;

        let temp = PrivateFixture::new();
        let security = private_directory_security().unwrap();
        let mut ace = std::ptr::null_mut();
        // SAFETY: generated private ACL has one initialized current-user ACE.
        assert_ne!(unsafe { GetAce(security.acl, 0, &mut ace) }, 0);
        // SAFETY: GetAce returned the generated ACL's live ACCESS_ALLOWED_ACE.
        // Keep all creation rights but remove DELETE so parent hardening would
        // observably change an inheriting child's ACL if it propagated.
        unsafe {
            (*ace.cast::<ACCESS_ALLOWED_ACE>()).Mask &= !DELETE;
        }
        let parent = open_private_directory(temp.path(), true).unwrap();
        // SAFETY: this fixture owns the parent and the restricted DACL remains
        // current-user-only. No children exist yet to receive propagation.
        assert_eq!(
            unsafe {
                SetSecurityInfo(
                    parent.0,
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    security.acl,
                    std::ptr::null_mut(),
                )
            },
            0
        );
        drop(parent);
        let child = temp.path().join("untargeted");
        std::fs::create_dir(&child).unwrap();
        let before = dacl_bytes(&child);
        ensure_private_directory(temp.path()).unwrap();
        assert_eq!(dacl_bytes(&child), before);
    }
}
