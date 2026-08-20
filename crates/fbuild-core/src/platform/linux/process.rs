use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::os::fd::AsFd;

use crate::path::NormalizedPath;
use crate::platform::process::{DetachedEnvironment, Termination};

pub(crate) fn configure_tokio_owner_death(
    command: &mut tokio::process::Command,
) -> std::io::Result<()> {
    // SAFETY: the pre-exec closure calls only async-signal-safe libc operations,
    // performs no allocation, and exits immediately if the parent already died.
    unsafe {
        command.pre_exec(|| {
            if setpgid(0, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if prctl(PR_SET_PDEATHSIG, SIGKILL) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if getppid() == 1 {
                _exit(1);
            }
            Ok(())
        });
    }
    Ok(())
}

pub(crate) fn after_tokio_spawn(_child: &tokio::process::Child) -> std::io::Result<()> {
    Ok(())
}

pub(crate) fn spawn_detached(
    command: &mut std::process::Command,
    stderr: Option<&std::fs::File>,
    environment: DetachedEnvironment,
) -> std::io::Result<u32> {
    let stderr = match stderr {
        Some(file) => running_process::DaemonStdioSource::Fd(file.as_fd()),
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
    std::fs::read_link(format!("/proc/{pid}/exe"))
        .ok()
        .map(Into::into)
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
const PR_SET_PDEATHSIG: i32 = 1;

unsafe extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
    fn setpgid(pid: i32, pgid: i32) -> i32;
    fn prctl(option: i32, ...) -> i32;
    fn getppid() -> i32;
    fn _exit(status: i32) -> !;
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
    use std::os::unix::fs::PermissionsExt;

    let path = directory.join("fbuild_1219_probe");
    std::fs::write(&path, "#!/bin/sh\necho overlay-marker\n")?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    Ok(super::super::process::PathProbe {
        path: path.into(),
        bare_args: vec!["fbuild_1219_probe".to_string()],
    })
}
