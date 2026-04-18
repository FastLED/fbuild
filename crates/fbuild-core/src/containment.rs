//! Process containment: every child process spawned by the daemon dies
//! with the daemon, including grandchildren forked by the child.
//!
//! # Why
//!
//! Without containment, a daemon that is SIGKILLed (or whose console
//! window is closed on Windows) leaves behind orphaned compiler /
//! linker / esptool / qemu / simavr / node / npm processes. On Windows
//! those orphans also leak their `bash.exe` / `conhost.exe` /
//! `OpenConsole.exe` wrappers; on Linux/macOS they re-parent to init.
//!
//! # How
//!
//! * **Windows** — a single Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
//!   When the daemon process exits for any reason, Windows closes the
//!   job handle and kills every assigned process atomically. Children
//!   are assigned to the job via `AssignProcessToJobObject` after spawn.
//! * **Linux** — children are placed in a new process group with
//!   `setpgid(0, 0)` and request `PR_SET_PDEATHSIG(SIGKILL)` so the
//!   kernel sends SIGKILL when the daemon thread exits. On `Drop` the
//!   group issues `killpg(SIGKILL)` as a backstop.
//! * **macOS** — process groups only; `prctl` is not available. The
//!   drop-time `killpg` is the primary mechanism.
//!
//! # Wiring
//!
//! The daemon binary initialises the process-wide group at startup via
//! [`init_global_containment`]. Every subprocess the daemon spawns goes
//! through:
//!
//! * [`fbuild_core::subprocess::run_command`](super::subprocess::run_command)
//!   — the central blocking helper used by compilers, linkers, esptool,
//!   avrdude, addr2line, and most emulator setup code.
//! * [`spawn_contained`] — direct `std::process::Command` spawns that
//!   don't go through `run_command` (e.g. zccache daemon startup).
//! * [`tokio_spawn::spawn_contained`] — long-running async spawns in
//!   the emulator handlers (QEMU, simavr, node/avr8js).
//!
//! Processes that must intentionally outlive the daemon — currently
//! only the daemon itself, spawned by the CLI and PyO3 bindings — use
//! [`spawn_detached`].
//!
//! Based on the `ContainedProcessGroup` / `Containment` primitives in
//! <https://github.com/zackees/running-process>. See FastLED/fbuild#32.

use std::process::{Child, Command};
use std::sync::OnceLock;

use running_process_core::{ContainedChild, ContainedProcessGroup, Containment};

/// Global process-wide containment group. Initialised once by the
/// daemon; remains `None` in non-daemon contexts (CLI binary, tests).
///
/// When `None`, spawn helpers fall back to uncontained spawning — the
/// same behaviour as before this feature landed. That keeps the CLI
/// and tests working without forcing every binary to manage a job
/// object.
static GLOBAL_GROUP: OnceLock<ContainedProcessGroup> = OnceLock::new();

/// Initialise the process-wide containment group.
///
/// Idempotent: a second call from the same process is a no-op. Call
/// this as early as possible from the daemon's `main` so the group
/// outlives every subprocess the daemon could possibly spawn.
///
/// The `originator` tag is propagated to child processes via the
/// `RUNNING_PROCESS_ORIGINATOR` env var so orphaned processes can be
/// correlated back to a specific daemon instance after a crash.
pub fn init_global_containment(originator: &str) -> std::io::Result<()> {
    if GLOBAL_GROUP.get().is_some() {
        return Ok(());
    }
    let group = ContainedProcessGroup::with_originator(originator)?;
    // OnceLock::set returns Err if another thread raced and set it
    // first; in that case the other value is equivalent — silently OK.
    let _ = GLOBAL_GROUP.set(group);
    Ok(())
}

/// True if the global containment group has been initialised in this
/// process. Primarily useful for tests.
pub fn is_initialised() -> bool {
    GLOBAL_GROUP.get().is_some()
}

/// Spawn a `std::process::Command` inside the global containment group.
///
/// Falls back to uncontained `Command::spawn` when no global group has
/// been initialised (non-daemon binaries).
pub fn spawn_contained(command: &mut Command) -> std::io::Result<Child> {
    match GLOBAL_GROUP.get() {
        Some(group) => {
            let ContainedChild { child, .. } =
                group.spawn_with_containment(command, Containment::Contained)?;
            Ok(child)
        }
        None => command.spawn(),
    }
}

/// Spawn a `std::process::Command` without containment. Intended for
/// processes that must outlive the daemon (the daemon itself, spawned
/// by the CLI / PyO3 bindings).
pub fn spawn_detached(command: &mut Command) -> std::io::Result<Child> {
    match GLOBAL_GROUP.get() {
        Some(group) => {
            let ContainedChild { child, .. } =
                group.spawn_with_containment(command, Containment::Detached)?;
            Ok(child)
        }
        None => command.spawn(),
    }
}

// ---------------------------------------------------------------------------
// tokio integration
// ---------------------------------------------------------------------------

/// Tokio-compatible containment helpers.
///
/// `tokio::process::Command` doesn't expose its inner
/// `std::process::Command`, so `ContainedProcessGroup::spawn` can't be
/// used directly. The helpers in this module reproduce the same
/// behaviour:
///
/// * On Unix the `pre_exec` hook (`CommandExt`, which tokio re-exposes)
///   performs the same `setpgid` + `PR_SET_PDEATHSIG` dance the core
///   crate does.
/// * On Windows the child is assigned to a parallel Job Object owned by
///   this module, configured with the same
///   `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` flag, so daemon death still
///   kills every tokio-spawned child.
///
/// Both job handles (the std-path one inside `running-process-core`,
/// and the one here) live for the lifetime of the daemon process and
/// are closed automatically when the daemon exits, triggering
/// kill-on-close for their respective children.
pub mod tokio_spawn {
    /// Spawn a `tokio::process::Command` inside the global containment
    /// group. Falls back to an uncontained spawn when no group is
    /// initialised.
    pub fn spawn_contained(
        command: &mut tokio::process::Command,
    ) -> std::io::Result<tokio::process::Child> {
        if !super::is_initialised() {
            return command.spawn();
        }
        configure(command);
        let child = command.spawn()?;
        post_spawn(&child)?;
        Ok(child)
    }

    /// Configure a tokio command with the containment pre-spawn hooks.
    ///
    /// On Unix this installs a `pre_exec` closure. On Windows this is a
    /// no-op — the Job Object assignment happens post-spawn in
    /// [`post_spawn`].
    pub fn configure(command: &mut tokio::process::Command) {
        #[cfg(unix)]
        {
            if !super::is_initialised() {
                return;
            }
            // SAFETY: the closure only calls async-signal-safe libc
            // entries (`setpgid`, `prctl`, `getppid`, `_exit`) and does
            // not allocate.
            unsafe {
                command.pre_exec(|| {
                    if libc::setpgid(0, 0) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    #[cfg(target_os = "linux")]
                    {
                        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1 {
                            return Err(std::io::Error::last_os_error());
                        }
                        if libc::getppid() == 1 {
                            // Parent already exited between fork() and
                            // prctl(); don't orphan this child to init.
                            libc::_exit(1);
                        }
                    }
                    Ok(())
                });
            }
        }
        #[cfg(not(unix))]
        {
            let _ = command;
        }
    }

    /// Assign the already-spawned child to the containment Job Object
    /// (Windows) or no-op (Unix, where `pre_exec` did the work).
    pub fn post_spawn(child: &tokio::process::Child) -> std::io::Result<()> {
        #[cfg(windows)]
        {
            if !super::is_initialised() {
                return Ok(());
            }
            let Some(raw) = child.raw_handle() else {
                // Already reaped — nothing to contain.
                return Ok(());
            };
            super::windows_job::assign(raw)
        }
        #[cfg(not(windows))]
        {
            let _ = child;
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Windows-only: parallel Job Object used for tokio spawns.
// ---------------------------------------------------------------------------
#[cfg(windows)]
mod windows_job {
    use std::sync::OnceLock;

    type Handle = *mut std::ffi::c_void;

    /// Wrapper around a raw `HANDLE` that is `Send + Sync`. The handle
    /// is owned for the lifetime of the process — we deliberately do
    /// not implement `Drop`, since closing the job handle would kill
    /// every assigned child. The daemon process exit is what triggers
    /// the kill-on-close semantics we want.
    #[derive(Debug, Clone, Copy)]
    struct JobHandle(Handle);

    unsafe impl Send for JobHandle {}
    unsafe impl Sync for JobHandle {}

    static JOB: OnceLock<JobHandle> = OnceLock::new();

    const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;
    const JOB_OBJECT_LIMIT_BREAKAWAY_OK: u32 = 0x0800;
    const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct IoCounters {
        read_operation_count: u64,
        write_operation_count: u64,
        other_operation_count: u64,
        read_transfer_count: u64,
        write_transfer_count: u64,
        other_transfer_count: u64,
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct JobObjectBasicLimitInformation {
        per_process_user_time_limit: i64,
        per_job_user_time_limit: i64,
        limit_flags: u32,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct JobObjectExtendedLimitInformation {
        basic_limit_information: JobObjectBasicLimitInformation,
        io_info: IoCounters,
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateJobObjectW(security_attrs: *mut std::ffi::c_void, name: *const u16) -> Handle;
        fn SetInformationJobObject(
            job: Handle,
            info_class: i32,
            info: *mut std::ffi::c_void,
            info_len: u32,
        ) -> i32;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
    }

    fn ensure_job() -> std::io::Result<Handle> {
        if let Some(h) = JOB.get() {
            return Ok(h.0);
        }
        let job = unsafe { CreateJobObjectW(std::ptr::null_mut(), std::ptr::null()) };
        if job.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let mut info = JobObjectExtendedLimitInformation::default();
        info.basic_limit_information.limit_flags =
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK;
        let ok = unsafe {
            SetInformationJobObject(
                job,
                JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                &mut info as *mut _ as *mut std::ffi::c_void,
                std::mem::size_of::<JobObjectExtendedLimitInformation>() as u32,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let _ = JOB.set(JobHandle(job));
        Ok(JOB.get().expect("just set").0)
    }

    pub(super) fn assign(process_handle: Handle) -> std::io::Result<()> {
        let job = ensure_job()?;
        let ok = unsafe { AssignProcessToJobObject(job, process_handle) };
        if ok == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_contained_without_init_falls_back_to_uncontained() {
        // When no global group is installed, the helper should still be
        // able to spawn processes — this preserves behaviour for the
        // CLI binary and for unit tests.
        let mut cmd = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.args(["/C", "echo", "hello"]);
            c
        } else {
            let mut c = Command::new("echo");
            c.arg("hello");
            c
        };
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
        let mut child = spawn_contained(&mut cmd).expect("spawn");
        let _ = child.wait();
    }

    #[test]
    fn is_initialised_is_bool() {
        // Compile-time sanity check that the accessor exists and returns
        // a `bool`. We intentionally do not call
        // `init_global_containment` here because the global is shared
        // across all tests in this binary.
        let _: bool = is_initialised();
    }
}
