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
//! * **Linux** — each child is placed in its own new process group with
//!   `setpgid(0, 0)` and requests `PR_SET_PDEATHSIG(SIGKILL)` so the
//!   kernel sends SIGKILL when the daemon thread exits. Per-child groups
//!   avoid the EPERM that the pre-publication
//!   `ContainedProcessGroup::spawn_with_containment` (since removed from
//!   `running-process` 4.0) hit when a second child tried to join a
//!   stale, already-exited first child's pgid (see issue #129).
//! * **macOS** — `prctl` is not available. Each child gets a fresh
//!   process group; there is no drop-time `killpg` backstop because the
//!   global group is a `OnceLock<...>` that never drops. This is the
//!   same coverage gap that existed before this fix — macOS containment
//!   is best-effort, and improving it is tracked separately.
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
//! <https://github.com/zackees/running-process>. On Unix, we
//! deliberately bypass `ContainedProcessGroup::spawn_with_containment`'s
//! shared-pgid behaviour and apply the `setpgid(0, 0)` + `prctl` pattern
//! per-child ourselves — see the module-level "How" section for why.
//! See FastLED/fbuild#32, #129.

use std::process::{Child, Command};
use std::sync::OnceLock;

use running_process::{ContainedProcessGroup, ORIGINATOR_ENV_VAR};

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
///
/// **Windows**: spawn directly, then assign the child handle to a Job
/// Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`
/// (via the private `windows_job::assign` helper) — the same containment
/// mechanism the pre-publication `running-process-core` rev used internally,
/// reimplemented locally since the published `running-process` 4.0 API no
/// longer exposes `spawn_with_containment(_, Containment::Contained)`.
///
/// **Unix**: installs a per-child `pre_exec` hook that creates a new
/// process group (`setpgid(0, 0)`) and, on Linux, requests
/// `PR_SET_PDEATHSIG(SIGKILL)`. We deliberately do not call
/// `ContainedProcessGroup::spawn` here because the per-child pgid
/// approach sidesteps an EPERM race that hit the second spawn after the
/// first child (the pgid leader) had exited — the root cause of
/// FastLED/fbuild#129. The Linux kernel's `PR_SET_PDEATHSIG` still
/// enforces the "child dies with daemon" contract; macOS relies on
/// process-group-leader death alone.
pub fn spawn_contained(command: &mut Command) -> std::io::Result<Child> {
    #[cfg(windows)]
    {
        let Some(group) = GLOBAL_GROUP.get() else {
            return command.spawn();
        };
        inject_originator_env(command, group);
        let mut child = command.spawn()?;
        use std::os::windows::io::AsRawHandle;
        if let Err(e) = windows_job::assign(child.as_raw_handle()) {
            // The atomic spawn+assign that `ContainedProcessGroup::spawn_with_containment`
            // used to provide is gone in `running-process` 4.0. If assign
            // fails after spawn succeeds, kill the orphan so the caller
            // can't leak an uncontained child by accident.
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
        Ok(child)
    }
    #[cfg(unix)]
    {
        let Some(group) = GLOBAL_GROUP.get() else {
            return command.spawn();
        };
        inject_originator_env(command, group);
        unix_install_pre_exec(command);
        command.spawn()
    }
}

/// Spawn a `std::process::Command` without containment. Intended for
/// processes that must outlive the daemon (the daemon itself, spawned
/// by the CLI / PyO3 bindings).
pub fn spawn_detached(command: &mut Command) -> std::io::Result<Child> {
    #[cfg(windows)]
    {
        // Detached: no Job Object assignment so the child survives
        // when the daemon's job handle closes. We still inject the
        // originator env var for cross-process correlation.
        if let Some(group) = GLOBAL_GROUP.get() {
            inject_originator_env(command, group);
        }
        command.spawn()
    }
    #[cfg(unix)]
    {
        // Detached: create a new session so the child survives the
        // daemon thread that spawned it. Matches the upstream behaviour
        // but without joining any shared pgid.
        let Some(group) = GLOBAL_GROUP.get() else {
            return command.spawn();
        };
        inject_originator_env(command, group);
        unix_install_detached_pre_exec(command);
        command.spawn()
    }
}

/// Mirror of `ContainedProcessGroup::inject_originator_env`: stamp
/// `RUNNING_PROCESS_ORIGINATOR=TOOL:PID` onto the command's env. We do
/// this manually because the published `running-process` 4.0 API only
/// exposes it via `ContainedProcessGroup::spawn` (which returns its own
/// `SpawnedChild`, not a `std::process::Child` — see #32).
fn inject_originator_env(command: &mut Command, group: &ContainedProcessGroup) {
    if let Some(value) = group.originator_value() {
        command.env(ORIGINATOR_ENV_VAR, value);
    }
}

/// Install a `pre_exec` hook that puts the child in a fresh process
/// group (Unix) and, on Linux, asks the kernel to SIGKILL the child
/// when the spawning thread exits.
///
/// Per-child pgid avoids the EPERM race from
/// [`ContainedProcessGroup::spawn_with_containment`] where a second
/// spawn tries to join a stale first-child pgid.
#[cfg(unix)]
fn unix_install_pre_exec(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: the closure only calls async-signal-safe libc entries
    // (`setpgid`, `prctl`, `getppid`, `_exit`) and does not allocate.
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
                    // Parent already exited between fork() and prctl();
                    // don't orphan this child to init.
                    libc::_exit(1);
                }
            }
            Ok(())
        });
    }
}

#[cfg(unix)]
fn unix_install_detached_pre_exec(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: `setsid` is async-signal-safe.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
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
/// Both job handles (the std-path one inside `running-process`,
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
        let Some(group) = super::GLOBAL_GROUP.get() else {
            return command.spawn();
        };
        if let Some(value) = group.originator_value() {
            command.env(super::ORIGINATOR_ENV_VAR, value);
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
            // allow-direct-spawn: this IS the containment module's own test of spawn_contained.
            let mut c = Command::new("cmd");
            c.args(["/C", "echo", "hello"]);
            c
        } else {
            // allow-direct-spawn: this IS the containment module's own test of spawn_contained.
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

    /// Regression: FastLED/fbuild#129 — two consecutive
    /// `spawn_contained` calls must both succeed after the first child
    /// has exited. The previous implementation (shared-pgid via
    /// `ContainedProcessGroup::spawn_with_containment`) recorded the
    /// first child's PID as the pgid and then tried to `setpgid(0,
    /// first_pid)` on the second child, which fails with EPERM once
    /// the first child has exited and been reaped.
    ///
    /// Per-child pgids (`setpgid(0, 0)`) + `PR_SET_PDEATHSIG` on Linux
    /// avoids the stale-pgid dependency entirely. Windows uses Job
    /// Object assignment, which is stateless and has no analogous
    /// failure mode.
    #[test]
    fn sequential_contained_spawns_do_not_fail_with_eperm() {
        // Install the global group so we exercise the contained path
        // rather than the uncontained fallback. Idempotent.
        init_global_containment("FBUILD-UNIT-TEST").expect("init_global_containment");
        assert!(is_initialised(), "global group must be initialised");

        // First spawn: short-lived command, wait for it to exit before
        // issuing the second spawn. This is exactly the shape of the
        // AVR build's "gcc -dumpversion then compile" sequence that
        // reproduces the original bug.
        let build_cmd = || {
            let mut cmd = if cfg!(windows) {
                // allow-direct-spawn: regression test for this module's own containment behaviour.
                let mut c = Command::new("cmd");
                c.args(["/C", "echo", "ok"]);
                c
            } else {
                // allow-direct-spawn: regression test for this module's own containment behaviour.
                let mut c = Command::new("echo");
                c.arg("ok");
                c
            };
            cmd.stdin(std::process::Stdio::null());
            cmd.stdout(std::process::Stdio::null());
            cmd.stderr(std::process::Stdio::null());
            cmd
        };

        let mut first = build_cmd();
        let mut first_child = spawn_contained(&mut first).expect("first spawn");
        let first_status = first_child.wait().expect("first wait");
        assert!(first_status.success(), "first child should succeed");

        // Small pause so the reaped pid and its (now defunct) pgroup
        // have time to be torn down by the kernel. On the pre-fix
        // code path this made the second spawn's `setpgid(0, stale)`
        // fail with EPERM with very high probability.
        std::thread::sleep(std::time::Duration::from_millis(50));

        let mut second = build_cmd();
        let mut second_child =
            spawn_contained(&mut second).expect("second spawn must not fail with EPERM");
        let second_status = second_child.wait().expect("second wait");
        assert!(second_status.success(), "second child should succeed");
    }
}
