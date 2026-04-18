//! Integration test for process containment (FastLED/fbuild#32).
//!
//! Spawns the `containment_harness` binary in `parent` mode. The parent
//! installs the global `ContainedProcessGroup`, spawns a contained
//! child, and the child spawns a grandchild. The parent writes
//! `<parent-pid> <child-pid> <grandchild-pid>\n` to stdout, then sleeps.
//!
//! The test driver then hard-kills **only** the parent. Thanks to
//! containment:
//!
//! * On **Windows** the Job Object's `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`
//!   semantics fire the moment the parent's process handle is reaped and
//!   the job handle goes away, killing every assigned descendant.
//! * On **Linux** the kernel's `PR_SET_PDEATHSIG(SIGKILL)` on each child
//!   and the drop-time `killpg(SIGKILL)` backstop kill the group.
//! * On **macOS** the drop-time `killpg(SIGKILL)` kills the group.
//!
//! The test polls for the child and grandchild PIDs and asserts both
//! are gone within a few seconds.
//!
//! This test is marked `#[ignore]` because it:
//!   1. Hard-kills processes, which CI runners can flag as noisy.
//!   2. Requires the `containment_harness` binary, which is built on
//!      demand by `CARGO_BIN_EXE_*`.
//!
//! Run explicitly with:
//! ```bash
//! uv run cargo test -p fbuild-daemon --test process_containment -- --ignored
//! ```

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
#[ignore = "spawns real subprocesses and issues hard-kills; run with --ignored"]
fn daemon_children_die_when_daemon_dies() {
    let harness = env!("CARGO_BIN_EXE_containment_harness");

    // Start the parent role.
    let mut parent = Command::new(harness)
        .arg("parent")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn parent");

    // Read one line of `<parent> <child> <grandchild>\n` from the
    // parent's stdout. The line is emitted only after the grandchild
    // has been spawned, so when we have the three PIDs we know the
    // whole tree is live.
    let stdout = parent.stdout.take().expect("parent stdout");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let read = reader.read_line(&mut line).expect("read parent line");
    assert!(read > 0, "parent did not emit PID line");

    let pids: Vec<u32> = line
        .split_whitespace()
        .map(|s| s.parse::<u32>().expect("pid parse"))
        .collect();
    assert_eq!(
        pids.len(),
        3,
        "expected three PIDs (parent child grandchild), got {:?}",
        pids
    );
    let parent_pid = pids[0];
    let child_pid = pids[1];
    let grandchild_pid = pids[2];

    // Sanity: every pid must be alive *right now*.
    assert!(
        pid_alive(child_pid),
        "child {} is not alive before kill",
        child_pid
    );
    assert!(
        pid_alive(grandchild_pid),
        "grandchild {} is not alive before kill",
        grandchild_pid
    );

    // Hard-kill the parent.
    kill_hard(parent_pid).expect("hard-kill parent");

    // Wait for the parent to be reaped. This is necessary on Windows
    // because the Job Object's kill-on-close only fires after the job
    // handle goes away, which requires the parent process to have fully
    // exited and its HANDLE to be closed by the test driver's `Child`.
    let _ = parent.wait();

    // Poll for up to 10 s: after containment fires, both child and
    // grandchild must be gone.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let child_gone = !pid_alive(child_pid);
        let grand_gone = !pid_alive(grandchild_pid);
        if child_gone && grand_gone {
            return; // success
        }
        if Instant::now() >= deadline {
            panic!(
                "containment failed: child {} alive={}, grandchild {} alive={}",
                child_pid,
                pid_alive(child_pid),
                grandchild_pid,
                pid_alive(grandchild_pid),
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

// ---------------------------------------------------------------------------
// OS-specific PID probes and hard-kill
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    // `kill(pid, 0)` is a probe — returns 0 if the pid exists and we
    // have permission, -1/ESRCH otherwise.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(windows)]
fn pid_alive(pid: u32) -> bool {
    // OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION) succeeds for any
    // running process; fails for a dead / non-existent PID. Also check
    // the exit code — a handle to a process that has exited but not
    // yet been reaped will still open successfully but report exited.
    type Handle = *mut std::ffi::c_void;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;
    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> Handle;
        fn CloseHandle(handle: Handle) -> i32;
        fn GetExitCodeProcess(handle: Handle, exit_code: *mut u32) -> i32;
    }
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        let mut code: u32 = 0;
        let ok = GetExitCodeProcess(h, &mut code as *mut u32);
        CloseHandle(h);
        ok != 0 && code == STILL_ACTIVE
    }
}

#[cfg(unix)]
fn kill_hard(pid: u32) -> std::io::Result<()> {
    let rc = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn kill_hard(pid: u32) -> std::io::Result<()> {
    // `taskkill /F` is the standard Windows hard-kill and works for
    // arbitrary PIDs without DLL shenanigans.
    let status = Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "taskkill /F /PID {} exited with {:?}",
            pid,
            status.code()
        )))
    }
}
