//! Cross-platform, PID-recycling-safe process identity primitives.
//!
//! Copied from soldr's daemon lifecycle semantics
//! (`.extern-repos/soldr/crates/soldr-daemon/src/daemon/lifecycle.rs`):
//! before ever signalling a PID recorded in a file, verify (a) the PID is
//! still alive, AND (b) its running executable image's file stem matches
//! what we expect. A PID can be recycled by the OS between when a file was
//! written and when it's read back, so liveness alone is not enough — an
//! unrelated process could receive a `SIGKILL` / `TerminateProcess` meant
//! for a long-dead daemon.
//!
//! [`pid_exe_stem_matches`] **fails closed**: if the process image can't be
//! inspected (permission denied, already exited, platform probe failure),
//! it returns `false` rather than assuming a match. Callers must never
//! signal a PID whose identity they could not positively verify.

use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Is `pid` currently alive?
///
/// Unix: `kill(pid, 0)` — delivers no signal, just probes existence +
/// permission. Windows: `OpenProcess` + `GetExitCodeProcess`, checking for
/// `STILL_ACTIVE`.
#[cfg(unix)]
pub fn pid_is_alive(pid: u32) -> bool {
    // SAFETY: kill(pid, 0) is a well-defined liveness probe — no signal is
    // delivered, the syscall just returns 0 if the pid exists and the
    // caller has permission to signal it.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(windows)]
#[allow(clippy::upper_case_acronyms, non_snake_case)]
pub fn pid_is_alive(pid: u32) -> bool {
    use std::os::windows::raw::HANDLE;
    #[allow(clippy::upper_case_acronyms)]
    type DWORD = u32;
    #[allow(clippy::upper_case_acronyms)]
    type BOOL = i32;
    const PROCESS_QUERY_LIMITED_INFORMATION: DWORD = 0x1000;
    const STILL_ACTIVE: DWORD = 259;
    extern "system" {
        fn OpenProcess(desired_access: DWORD, inherit: BOOL, pid: DWORD) -> HANDLE;
        fn CloseHandle(h: HANDLE) -> BOOL;
        fn GetExitCodeProcess(h: HANDLE, code: *mut DWORD) -> BOOL;
    }
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        return false;
    }
    let mut code: DWORD = 0;
    let ok = unsafe { GetExitCodeProcess(h, &mut code) };
    unsafe { CloseHandle(h) };
    ok != 0 && code == STILL_ACTIVE
}

#[cfg(not(any(unix, windows)))]
pub fn pid_is_alive(_pid: u32) -> bool {
    false
}

/// Read the executable image path of a running process.
///
/// Linux: `/proc/<pid>/exe` (symlink read). macOS/BSD: `ps -o comm=`
/// (portable, no extra crate). Windows: `QueryFullProcessImageNameW`. Any
/// probe failure returns `None` — the caller treats that as "identity
/// unverified", never as "assume match".
#[cfg(target_os = "linux")]
pub fn pid_executable_path(pid: u32) -> Option<PathBuf> {
    let link = PathBuf::from(format!("/proc/{pid}/exe"));
    std::fs::read_link(link).ok()
}

#[cfg(all(unix, not(target_os = "linux")))]
pub fn pid_executable_path(pid: u32) -> Option<PathBuf> {
    let output = std::process::Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let image = String::from_utf8(output.stdout).ok()?;
    let image = image.trim();
    (!image.is_empty()).then(|| PathBuf::from(image))
}

#[cfg(windows)]
#[allow(clippy::upper_case_acronyms, non_snake_case)]
pub fn pid_executable_path(pid: u32) -> Option<PathBuf> {
    use std::os::windows::raw::HANDLE;
    #[allow(clippy::upper_case_acronyms)]
    type DWORD = u32;
    #[allow(clippy::upper_case_acronyms)]
    type BOOL = i32;
    type WCHAR = u16;
    const PROCESS_QUERY_LIMITED_INFORMATION: DWORD = 0x1000;
    extern "system" {
        fn OpenProcess(desired_access: DWORD, inherit: BOOL, pid: DWORD) -> HANDLE;
        fn CloseHandle(h: HANDLE) -> BOOL;
        fn QueryFullProcessImageNameW(
            h: HANDLE,
            flags: DWORD,
            buf: *mut WCHAR,
            size: *mut DWORD,
        ) -> BOOL;
    }
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        return None;
    }
    let mut buf: Vec<WCHAR> = vec![0; 1024];
    let mut size: DWORD = buf.len() as DWORD;
    let ok = unsafe { QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut size) };
    unsafe { CloseHandle(h) };
    if ok == 0 {
        return None;
    }
    let s = String::from_utf16_lossy(&buf[..size as usize]);
    (!s.is_empty()).then(|| PathBuf::from(s))
}

#[cfg(not(any(unix, windows)))]
pub fn pid_executable_path(_pid: u32) -> Option<PathBuf> {
    None
}

/// PID-recycling-safe identity gate: does `pid`'s running executable image
/// have file stem `expected_stem`?
///
/// **Fails closed**: an uninspectable image (permission denied, process
/// already gone, probe error) returns `false`. Case-insensitive on Windows
/// (`file.EXE` vs `file.exe`), case-sensitive elsewhere. Callers must gate
/// every signal (`terminate_pid`) on this check to avoid killing an
/// unrelated process that happens to have inherited a stale PID.
pub fn pid_exe_stem_matches(pid: u32, expected_stem: &str) -> bool {
    let Some(path) = pid_executable_path(pid) else {
        return false;
    };
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return false;
    };
    if cfg!(windows) {
        stem.eq_ignore_ascii_case(expected_stem)
    } else {
        stem == expected_stem
    }
}

/// Terminate `pid`: SIGTERM then, if still alive after ~3s, SIGKILL (Unix).
/// `TerminateProcess` (Windows, no graceful signal available). Callers MUST
/// have already verified [`pid_exe_stem_matches`] before calling this —
/// this function does not re-check identity, it just signals.
#[cfg(unix)]
pub fn terminate_pid(pid: u32) {
    // SAFETY: kill(2) with SIGTERM then (if needed) SIGKILL. The caller is
    // responsible for having verified the PID's identity beforehand.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
    if wait_for_pid_exit(pid, Duration::from_secs(3)) {
        return;
    }
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
}

#[cfg(windows)]
#[allow(clippy::upper_case_acronyms, non_snake_case)]
pub fn terminate_pid(pid: u32) {
    use std::os::windows::raw::HANDLE;
    #[allow(clippy::upper_case_acronyms)]
    type DWORD = u32;
    #[allow(clippy::upper_case_acronyms)]
    type BOOL = i32;
    const PROCESS_TERMINATE: DWORD = 0x0001;
    extern "system" {
        fn OpenProcess(desired_access: DWORD, inherit: BOOL, pid: DWORD) -> HANDLE;
        fn TerminateProcess(h: HANDLE, exit_code: DWORD) -> BOOL;
        fn CloseHandle(h: HANDLE) -> BOOL;
    }
    // SAFETY: OpenProcess for a caller-verified PID; TerminateProcess is the
    // Windows equivalent of SIGKILL — there is no graceful-signal analog.
    let h = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
    if h.is_null() {
        return;
    }
    unsafe {
        TerminateProcess(h, 1);
        CloseHandle(h);
    }
}

#[cfg(not(any(unix, windows)))]
pub fn terminate_pid(_pid: u32) {}

/// Poll until `pid` exits or `timeout` elapses. Returns `true` if the
/// process was observed dead within the timeout.
pub fn wait_for_pid_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !pid_is_alive(pid) {
            return true;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50).min(remaining));
    }
    !pid_is_alive(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_pid_is_alive() {
        assert!(pid_is_alive(std::process::id()));
    }

    #[test]
    fn dead_pid_is_not_alive() {
        // A large positive PID that is almost certainly not a running
        // process. NOT `u32::MAX`, which casts to `-1` (the "all
        // processes" wildcard) on Unix and would spuriously look alive.
        let dead = i32::MAX as u32;
        assert!(!pid_is_alive(dead));
    }

    #[test]
    fn exe_stem_fails_closed_for_wrong_stem() {
        // Our own PID is alive, but its exe stem is the test binary, not
        // some arbitrary expected name — must fail closed (false), never
        // assume a match.
        assert!(!pid_exe_stem_matches(
            std::process::id(),
            "definitely-not-our-test-binary-stem"
        ));
    }

    #[test]
    fn exe_stem_matches_current_exe_stem() {
        let current_exe = std::env::current_exe().expect("current exe");
        let stem = current_exe
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("current exe stem")
            .to_string();
        assert!(pid_exe_stem_matches(std::process::id(), &stem));
    }

    #[test]
    fn exe_stem_fails_closed_for_dead_pid() {
        let dead = i32::MAX as u32;
        assert!(!pid_exe_stem_matches(dead, "anything"));
    }

    #[test]
    fn wait_for_pid_exit_returns_true_for_already_dead_pid() {
        let dead = i32::MAX as u32;
        assert!(wait_for_pid_exit(dead, Duration::from_millis(50)));
    }
}
