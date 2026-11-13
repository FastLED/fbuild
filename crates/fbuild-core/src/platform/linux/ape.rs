//! Linux pieces of APE support: default cache directories and the sealed
//! `memfd` fallback used when no cache directory is usable (read-only home,
//! `noexec` `/tmp`, no runtime dir). The memfd is held open for the life of
//! the process and exec'd via `/proc/self/fd/N`.

use std::collections::HashMap;
use std::ffi::CString;
use std::fs::File;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::PermissionsExt;
use crate::path::NormalizedPath;
use std::sync::Mutex;

/// Default candidate directories, most durable first.
pub(crate) fn default_loader_dirs() -> Vec<NormalizedPath> {
    let env_dir = |key: &str| std::env::var_os(key).filter(|v| !v.is_empty()).map(NormalizedPath::new);
    let mut dirs = Vec::new();
    if let Some(cache) = env_dir("XDG_CACHE_HOME").or_else(|| env_dir("HOME").map(|h| h.join(".cache"))) {
        dirs.push(cache.join("fbuild").join("ape"));
    }
    if let Some(runtime) = env_dir("XDG_RUNTIME_DIR") {
        dirs.push(runtime.join("fbuild").join("ape"));
    }
    dirs
}

/// Whether `path` is one of this process's anonymous (memfd) executables.
/// Exact identity, not a prefix test: only paths handed out here qualify.
pub(crate) fn is_anonymous(path: &std::path::Path) -> bool {
    let guard = MEMFDS.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .as_ref()
        .is_some_and(|fds| fds.values().any(|file| fd_path(file).as_path() == path))
}

/// Last-resort executable with no filesystem home: a sealed memfd exec'd via
/// `/proc/self/fd/N`. Valid only for direct children of this process.
pub(crate) fn anonymous_executable(bytes: &[u8], name: &str) -> Option<NormalizedPath> {
    memfd_loader(bytes, name)
}

/// Sealed memfds kept open for the process lifetime, keyed by loader name.
static MEMFDS: Mutex<Option<HashMap<String, File>>> = Mutex::new(None);

fn memfd_loader(bytes: &[u8], name: &str) -> Option<NormalizedPath> {
    let mut guard = MEMFDS.lock().unwrap_or_else(|e| e.into_inner());
    let fds = guard.get_or_insert_with(HashMap::new);
    if let Some(file) = fds.get(name) {
        return Some(fd_path(file));
    }
    let file = {
        // The memfd is writable until sealed; keep it out of forked children.
        let _fork = crate::platform::process::exclusive_fork_guard();
        create_sealed_memfd(bytes, name)?
    };
    let path = fd_path(&file);
    // The child resolves this path before close-on-exec runs; without /proc
    // it cannot, so refuse rather than hand out a dead path.
    if !path.exists() {
        return None;
    }
    fds.insert(name.to_owned(), file);
    Some(path)
}

fn create_sealed_memfd(bytes: &[u8], name: &str) -> Option<File> {
    let cname = CString::new(name).ok()?;
    let base = libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING;
    // MFD_EXEC (Linux 6.3+) keeps the memfd executable under
    // `vm.memfd_noexec=1`; older kernels reject the flag with EINVAL.
    let fd = [base | libc::MFD_EXEC, base].into_iter().find_map(|flags| {
        // Raw syscall, not the libc wrapper: release builds link glibc 2.17,
        // which predates `memfd_create()` (added in glibc 2.27).
        // SAFETY: `cname` is NUL-terminated; flags are valid memfd flags.
        let fd = unsafe { libc::syscall(libc::SYS_memfd_create, cname.as_ptr(), flags) };
        (fd >= 0).then_some(fd as libc::c_int)
    })?;
    // SAFETY: `fd` is a freshly created descriptor owned by nobody else.
    let mut file = unsafe { File::from_raw_fd(fd) };
    file.write_all(bytes).ok()?;
    file.set_permissions(std::fs::Permissions::from_mode(0o500))
        .ok()?;
    let seals = libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE | libc::F_SEAL_SEAL;
    // SAFETY: `file` owns a valid memfd created with MFD_ALLOW_SEALING.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, seals) } != 0 {
        return None;
    }
    Some(file)
}

fn fd_path(file: &File) -> NormalizedPath {
    NormalizedPath::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::ape::install::install_in;
    use crate::platform::host::HostArch;
    use std::fs::OpenOptions;
    use std::path::Path;

    fn hello() -> NormalizedPath {
        NormalizedPath::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("data/ape-hello/hello.com"))
    }

    fn host_loader() -> Vec<u8> {
        let arch = crate::platform::host::current().arch();
        crate::platform::ape::extract_loader(&hello(), arch).expect("fixture embeds host loader")
    }

    fn run(loader: &Path, args: &[&str]) -> crate::subprocess::ToolOutput {
        let hello = hello();
        let mut argv = vec![loader.to_str().unwrap(), hello.to_str().unwrap()];
        argv.extend_from_slice(args);
        // Hostile child env: nothing on PATH, no TMPDIR, no HOME.
        crate::subprocess::run_command_blocking(
            &argv,
            None,
            Some(&[("PATH", "/nonexistent"), ("TMPDIR", "/nonexistent"), ("HOME", "/nonexistent")]),
            None,
        )
        .expect("loader must spawn")
    }

    #[test]
    fn memfd_loader_runs_real_image() {
        let loader = memfd_loader(&host_loader(), "ape-loader-test-memfd").expect("memfd");
        assert!(is_anonymous(&loader));
        let out = run(&loader, &["memfd"]);
        assert!(out.success(), "stderr: {}", out.stderr);
        assert_eq!(out.stdout, "hello world memfd\n");
        // Sealed: the running loader can't be rewritten underneath children.
        assert!(OpenOptions::new().write(true).open(&loader).and_then(|mut f| f.write_all(b"x")).is_err());
    }

    #[test]
    fn unusable_dirs_fall_back_to_memfd() {
        let root = tempfile::tempdir().unwrap();
        // A regular file as parent: uncreatable even for root, unlike a 0500
        // dir (root bypasses DAC permissions).
        let readonly = root.path().join("not-a-dir");
        std::fs::write(&readonly, b"").unwrap();
        let shared = root.path().join("shared");
        std::fs::create_dir(&shared).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
        let dirs = [
            NormalizedPath::new(readonly.join("cache")),
            NormalizedPath::new(&shared),
            NormalizedPath::from("/proc/fbuild-nope"),
        ];
        let loader =
            crate::platform::ape::materialize(&host_loader(), "ape-loader-test-fallback", &dirs).unwrap();
        assert!(is_anonymous(&loader), "got {}", loader.display());
        assert!(std::fs::read_dir(&shared).unwrap().next().is_none(), "nothing planted in shared dir");
        let out = run(&loader, &["fallback"]);
        assert_eq!(out.stdout, "hello world fallback\n", "stderr: {}", out.stderr);
    }

    #[test]
    fn symlinked_cache_dir_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("real")).unwrap();
        std::os::unix::fs::symlink(root.path().join("real"), root.path().join("link")).unwrap();
        assert!(install_in(&root.path().join("link"), &host_loader(), "l").is_none());
    }

    #[test]
    fn tampered_or_truncated_install_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = host_loader();
        let path = install_in(dir.path(), &bytes, "ape-loader-t").expect("install");
        for bad in [&b"#!/bin/sh\nexit 66\n"[..], &bytes[..bytes.len() / 2]] {
            std::fs::write(&path, bad).unwrap();
            assert_eq!(install_in(dir.path(), &bytes, "ape-loader-t"), Some(path.clone()));
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            let out = run(&path, &["repaired"]);
            assert_eq!(out.stdout, "hello world repaired\n", "stderr: {}", out.stderr);
        }
        // Lost exec bit is also repaired.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        install_in(dir.path(), &bytes, "ape-loader-t").unwrap();
        assert_ne!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o100, 0);
    }

    #[test]
    fn concurrent_installs_converge_without_partial_files() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("fresh/ape");
        let bytes = host_loader();
        let paths: Vec<_> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..32)
                .map(|_| s.spawn(|| install_in(&cache, &bytes, "ape-loader-race")))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(paths.iter().all(|p| p.as_deref() == Some(cache.join("ape-loader-race").as_path())));
        let entries: Vec<_> = std::fs::read_dir(&cache).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("ape-loader-race")], "no stray temp files");
        assert_eq!(
            std::fs::metadata(&cache).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn foreign_arch_loader_is_valid_but_not_host_runnable_choice() {
        // Both CPUs are present in the fat fixture; only the host one is used.
        assert!(crate::platform::ape::extract_loader(&hello(), HostArch::X86_64).is_some());
        assert!(crate::platform::ape::extract_loader(&hello(), HostArch::Aarch64).is_some());
        assert!(crate::platform::ape::extract_loader(&hello(), HostArch::Arm).is_none());
    }
}
