use std::os::unix::process::ExitStatusExt;

use crate::path::NormalizedPath;
use crate::platform::process::{DetachedEnvironment, Termination};

pub(crate) fn register_daemon_shutdown_handler(
    _shutdown_tx: tokio::sync::watch::Sender<bool>,
) -> std::io::Result<()> {
    Ok(())
}

pub(crate) async fn daemon_terminate_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    match signal(SignalKind::terminate()) {
        Ok(mut sigterm) => {
            sigterm.recv().await;
        }
        Err(error) => {
            tracing::warn!(
                "cannot install SIGTERM handler ({error}); SIGTERM will kill the daemon without a flush"
            );
            std::future::pending::<()>().await
        }
    }
}

/// Resident memory of `pid` from `proc_pidinfo(PROC_PIDTASKINFO)`.
///
/// The task port only resolves for processes this user may inspect, so a
/// denied or vanished pid yields `None`. macOS exposes no per-pid high-water
/// mark; `getrusage` covers this process only, and is used for that case
/// (FastLED/fbuild#1785).
pub(crate) fn resident_memory(pid: u32) -> Option<crate::platform::process::MemoryUsage> {
    let pid = i32::try_from(pid).ok()?;
    let mut info = std::mem::MaybeUninit::<libc::proc_taskinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
    // SAFETY: the buffer is `size` bytes and initialized before any read; the
    // kernel writes at most that many bytes into it.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTASKINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if written != size {
        return None;
    }
    // SAFETY: a full-size reply initializes the whole struct.
    let info = unsafe { info.assume_init() };
    let peak_resident_bytes = if pid == std::process::id() as i32 {
        peak_resident_memory_bytes()
    } else {
        None
    };
    Some(crate::platform::process::MemoryUsage {
        resident_bytes: info.pti_resident_size,
        peak_resident_bytes,
    })
}

/// This process's peak resident set from `RUSAGE_SELF`.
fn peak_resident_memory_bytes() -> Option<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `getrusage` writes one `rusage` into the provided pointer.
    let result = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    if result != 0 {
        return None;
    }
    // SAFETY: a zero return means the struct was filled.
    let usage = unsafe { usage.assume_init() };
    // macOS reports `ru_maxrss` in bytes; other Unix hosts use KiB.
    u64::try_from(usage.ru_maxrss).ok()
}

pub(crate) fn daemon_graceful_termination_budget() -> std::time::Duration {
    std::time::Duration::from_secs(crate::daemon_health::TERMINATE_EXIT_BUDGET.as_secs() + 1)
}

pub(crate) fn configure_tokio_owner_death(
    command: &mut tokio::process::Command,
) -> std::io::Result<()> {
    // SAFETY: the pre-exec closure calls only async-signal-safe `setpgid` and
    // performs no allocation or other non-reentrant work after fork.
    unsafe {
        command.pre_exec(|| {
            if setpgid(0, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(())
}

/// Owner-death here is not tied to the forking thread, so contained children
/// fork on the caller's thread as before (FastLED/fbuild#1628 is Linux-only).
pub(crate) fn spawn_tokio_on_owner_thread(
    command: &mut tokio::process::Command,
    spawn: fn(&mut tokio::process::Command) -> std::io::Result<tokio::process::Child>,
) -> std::io::Result<tokio::process::Child> {
    spawn(command)
}

pub(crate) fn after_tokio_spawn(_child: &tokio::process::Child) -> std::io::Result<()> {
    Ok(())
}

/// Unix resolves executables strictly via `PATH`; there is no system
/// fallback directory a child PATH cannot suppress.
pub(crate) fn system_exe_fallback_resolves(_exe_name: &str) -> bool {
    false
}

/// macOS has no single elevation mechanic; callers must not attempt this
/// and should route around it.
pub(crate) fn launch_elevated(
    _program: &std::ffi::OsStr,
    _parameters: &str,
) -> std::io::Result<super::super::process::ElevationOutcome> {
    Err(std::io::Error::other(
        "elevated process launch is a Windows-only mechanic",
    ))
}

pub(crate) fn spawn_detached(
    command: &mut std::process::Command,
    stderr: Option<&std::fs::File>,
    environment: DetachedEnvironment,
) -> std::io::Result<u32> {
    let stderr = match stderr {
        Some(file) => running_process::DaemonStdioSource::File(file),
        None => running_process::DaemonStdioSource::Null,
    };
    let child = running_process::spawn_daemon_with_stdio_and_env_policy(
        command,
        running_process::DaemonStdio {
            stdout: running_process::DaemonStdioSource::Null,
            stderr,
        },
        match environment {
            DetachedEnvironment::Inherit => running_process::EnvironmentPolicy::Inherit,
            DetachedEnvironment::Clear => running_process::EnvironmentPolicy::Clear,
        },
    )?;
    Ok(child.id())
}

pub(crate) fn pid_is_alive(pid: u32) -> bool {
    if pid > i32::MAX as u32 {
        return false;
    }
    // SAFETY: the range check above makes the cast lossless; signal 0 only probes.
    let result = unsafe { kill(pid as i32, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(EPERM)
}

pub(crate) fn pid_executable_path(pid: u32) -> Option<NormalizedPath> {
    // allow-direct-spawn: selected process implementation querying native PID image metadata.
    let output = std::process::Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let image = String::from_utf8(output.stdout).ok()?;
    let image = image.trim();
    (!image.is_empty()).then(|| NormalizedPath::from(image))
}

pub(crate) fn exe_stem_matches(actual: &str, expected: &str) -> bool {
    actual == expected
}

pub(crate) fn terminate_pid(pid: u32, termination: Termination) -> std::io::Result<()> {
    if pid > i32::MAX as u32 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "pid is outside the host process-id range",
        ));
    }
    let signal = match termination {
        Termination::Graceful => SIGTERM,
        Termination::Force => SIGKILL,
    };
    // SAFETY: both facade and local checks make the PID cast lossless, and the
    // signal is one of the two constants selected above.
    let result = unsafe { kill(pid as i32, signal) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

const EPERM: i32 = 1;
const SIGKILL: i32 = 9;
const SIGTERM: i32 = 15;

unsafe extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
    fn setpgid(pid: i32, pgid: i32) -> i32;
}

pub(crate) fn exit_code(status: std::process::ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| -status.signal().unwrap_or(1))
}

pub(crate) fn command_environment(
    _program: &str,
    overlay: Option<&[(&str, &str)]>,
) -> Option<Vec<(String, String)>> {
    overlay.filter(|values| !values.is_empty()).map(|values| {
        let mut environment: std::collections::BTreeMap<String, String> =
            std::env::vars().collect();
        environment.extend(
            values
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string())),
        );
        environment.into_iter().collect()
    })
}

#[cfg(test)]
pub(crate) fn create_path_probe(directory: &std::path::Path) -> std::io::Result<super::super::process::PathProbe> {
    let path = directory.join("fbuild_1219_probe");
    std::fs::write(&path, "#!/bin/sh\necho overlay-marker\n")?;
    super::fs::set_executable(&path)?;
    Ok(super::super::process::PathProbe {
        path: path.into(),
        bare_args: vec!["fbuild_1219_probe".to_string()],
    })
}
