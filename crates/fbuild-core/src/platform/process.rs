//! Neutral process, containment, and exit-interpretation APIs.

use std::fs::File;
use std::process::{ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::path::NormalizedPath;

static CONTAINMENT: OnceLock<running_process::ContainedProcessGroup> = OnceLock::new();

/// Native termination strength requested by caller-owned lifecycle policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Termination {
    /// Ask the process to exit cleanly where the host supports that operation.
    Graceful,
    /// Terminate the process immediately.
    Force,
}

/// Environment inheritance policy for a detached child.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DetachedEnvironment {
    /// Preserve the caller's environment, including command-specific overrides.
    Inherit,
    /// Start from an empty environment and retain only command-specific overrides.
    Clear,
}

/// Host-neutral standard-stream configuration for a contained child.
pub struct ContainedStdio<'a> {
    pub stdin: StdioSource<'a>,
    pub stdout: StdioSource<'a>,
    pub stderr: StdioSource<'a>,
}

impl Default for ContainedStdio<'_> {
    fn default() -> Self {
        Self {
            stdin: StdioSource::Null,
            stdout: StdioSource::Parent,
            stderr: StdioSource::Parent,
        }
    }
}

/// One standard stream supplied to a contained child.
pub enum StdioSource<'a> {
    Null,
    Parent,
    Pipe,
    #[doc(hidden)]
    _Lifetime(std::marker::PhantomData<&'a ()>),
}

/// Opaque handle for a contained child. Dropping it terminates the child tree.
pub struct ContainedChild {
    inner: running_process::SpawnedChild,
}

impl ContainedChild {
    pub fn id(&self) -> u32 {
        self.inner.id()
    }

    pub fn kill(&mut self) -> std::io::Result<()> {
        self.inner.kill()
    }

    pub fn wait(&mut self) -> std::io::Result<i32> {
        self.inner.wait()
    }

    pub fn try_wait(&mut self) -> std::io::Result<Option<i32>> {
        self.inner.try_wait()
    }

    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.inner.stdin.take()
    }

    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.inner.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.inner.stderr.take()
    }
}

/// Configure the process-wide originator applied to contained children.
pub fn init_containment(originator: &str) -> std::io::Result<()> {
    if CONTAINMENT.get().is_some() {
        return Ok(());
    }
    let group = running_process::ContainedProcessGroup::with_originator(originator)?;
    let _ = CONTAINMENT.set(group);
    Ok(())
}

pub fn containment_is_initialized() -> bool {
    CONTAINMENT.get().is_some()
}

/// Spawn a synchronous child using running-process's contained process tree.
pub fn spawn_contained(
    command: &mut Command,
    stdio: ContainedStdio<'_>,
) -> std::io::Result<ContainedChild> {
    let stdio = running_process::SpawnStdio {
        stdin: into_running_stdio(stdio.stdin),
        stdout: into_running_stdio(stdio.stdout),
        stderr: into_running_stdio(stdio.stderr),
        ..running_process::SpawnStdio::default()
    };
    let inner = match CONTAINMENT.get() {
        Some(group) => group.spawn(command, stdio)?,
        None => running_process::spawn(command, stdio)?,
    };
    Ok(ContainedChild { inner })
}

fn into_running_stdio(source: StdioSource<'_>) -> running_process::StdioSource<'_> {
    match source {
        StdioSource::Null => running_process::StdioSource::Null,
        StdioSource::Parent => running_process::StdioSource::Parent,
        StdioSource::Pipe => running_process::StdioSource::Pipe,
        StdioSource::_Lifetime(_) => unreachable!("private lifetime marker"),
    }
}

/// Spawn a daemon that is detached from the caller and has sanitized handles.
///
/// The returned PID remains valid after the internal handle is dropped; callers
/// use the inspection and termination functions below for later lifecycle work.
pub fn spawn_detached(
    command: &mut Command,
    stderr: Option<&File>,
    environment: DetachedEnvironment,
) -> std::io::Result<u32> {
    super::selected::process::spawn_detached(command, stderr, environment)
}

/// Spawn a Tokio child with console suppression, kill-on-drop, and (after
/// [`init_containment`]) owner-death containment.
pub fn spawn_tokio_contained(
    command: &mut tokio::process::Command,
) -> std::io::Result<tokio::process::Child> {
    if let Some(group) = CONTAINMENT.get() {
        if let Some(value) = group.originator_value() {
            command.env(running_process::ORIGINATOR_ENV_VAR, value);
        }
        super::selected::process::configure_tokio_owner_death(command)?;
    }
    let child =
        running_process::spawn_tokio(command, running_process::TokioSpawnOptions::default())?;
    if CONTAINMENT.get().is_some() {
        super::selected::process::after_tokio_spawn(&child)?;
    }
    Ok(child)
}

pub fn pid_is_alive(pid: u32) -> bool {
    pid != 0 && super::selected::process::pid_is_alive(pid)
}

pub fn pid_executable_path(pid: u32) -> Option<NormalizedPath> {
    if pid == 0 {
        return None;
    }
    super::selected::process::pid_executable_path(pid)
}

/// Fail-closed executable identity check used before signaling recorded PIDs.
pub fn pid_exe_stem_matches(pid: u32, expected_stem: &str) -> bool {
    let Some(path) = pid_executable_path(pid) else {
        return false;
    };
    let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
        return false;
    };
    super::selected::process::exe_stem_matches(stem, expected_stem)
}

/// Perform one native termination operation. Escalation timing stays with the caller.
pub fn terminate_pid(pid: u32, termination: Termination) -> std::io::Result<()> {
    if pid == 0 || pid > i32::MAX as u32 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "pid is outside fbuild's supported process-id range",
        ));
    }
    super::selected::process::terminate_pid(pid, termination)
}

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

/// How an elevated program launch ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ElevationOutcome {
    /// The user declined the elevation prompt; nothing ran. Expected user
    /// control flow, not a failure.
    Declined,
    /// The elevated program ran to completion with this exit code.
    Completed(u32),
}

/// Launch `program` with `parameters` through the host's elevation prompt
/// (Windows UAC "Run as administrator"), hiding the elevated window, and
/// wait for it to exit. Callers keep all policy: who may be elevated,
/// with which arguments, and what the result means.
///
/// Fails closed on hosts without an elevation mechanic; callers on those
/// hosts must route around this rather than attempt it.
pub fn launch_elevated(
    program: &std::ffi::OsStr,
    parameters: &str,
) -> std::io::Result<ElevationOutcome> {
    super::selected::process::launch_elevated(program, parameters)
}

/// Whether the host resolves executables through a system fallback search
/// path that a child process's PATH cannot suppress (Windows consults
/// `%WINDIR%`, home of the `py` launcher; Unix resolves strictly via
/// `PATH`). Tools that assert "empty PATH ⇒ executable not found" must
/// skip their strict assertion when this returns true for the probed name.
pub fn system_exe_fallback_resolves(exe_name: &str) -> bool {
    super::selected::process::system_exe_fallback_resolves(exe_name)
}

/// Normalize native signal/exception exits to fbuild's numeric exit-code contract.
pub fn exit_code(status: ExitStatus) -> i32 {
    super::selected::process::exit_code(status)
}

/// Bridge native daemon-shutdown notifications into the shared Tokio watch
/// channel. Hosts without an additional native notification source are a no-op.
pub fn register_daemon_shutdown_handler(
    shutdown_tx: tokio::sync::watch::Sender<bool>,
) -> std::io::Result<()> {
    super::selected::process::register_daemon_shutdown_handler(shutdown_tx)
}

/// Build the host-correct child environment while preserving caller overlays.
pub(crate) fn command_environment(
    program: &str,
    overlay: Option<&[(&str, &str)]>,
) -> Option<Vec<(String, String)>> {
    super::selected::process::command_environment(program, overlay)
}

#[cfg(test)]
pub(crate) struct PathProbe {
    pub path: NormalizedPath,
    pub bare_args: Vec<String>,
}

#[cfg(test)]
pub(crate) fn create_path_probe(directory: &std::path::Path) -> std::io::Result<PathProbe> {
    super::selected::process::create_path_probe(directory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn current_process_can_be_inspected_through_the_facade() {
        let pid = std::process::id();
        assert!(pid_is_alive(pid));
        let image = pid_executable_path(pid).expect("current process image");
        let stem = image
            .file_stem()
            .and_then(|value| value.to_str())
            .expect("current process image stem");
        assert!(pid_exe_stem_matches(pid, stem));
    }

    #[test]
    fn invalid_pids_fail_closed() {
        assert!(!pid_is_alive(0));
        assert!(pid_executable_path(0).is_none());
        assert!(!pid_exe_stem_matches(0, "fbuild-daemon"));
        assert!(wait_for_pid_exit(0, Duration::ZERO));
    }

    #[test]
    fn termination_modes_are_neutral_values() {
        assert_ne!(Termination::Graceful, Termination::Force);
    }

    #[test]
    fn termination_rejects_pid_values_that_would_wrap_on_posix() {
        let error = terminate_pid(u32::MAX, Termination::Force)
            .expect_err("oversized PID must fail before native dispatch");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }
}
