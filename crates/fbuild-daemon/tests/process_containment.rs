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
//! soldr cargo test -p fbuild-daemon --test process_containment -- --ignored
//! ```

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Bounded `Child::wait()` (FastLED/fbuild#806).
///
/// The audit flagged `parent.wait()` after `kill_hard(parent_pid)` because
/// if `taskkill /F` fails silently on Windows (AV interference, permissions)
/// `wait()` blocks forever — the polling-loop deadline further down only
/// fires after that wait returns. This helper polls `try_wait` with its own
/// deadline and force-kills the child handle if the deadline passes.
fn wait_with_timeout(child: &mut Child, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => return false,
        }
    }
}

#[test]
#[ignore = "spawns real subprocesses and issues hard-kills; run with --ignored"]
fn daemon_children_die_when_daemon_dies() {
    let harness = env!("CARGO_BIN_EXE_containment_harness");

    // Start the parent role.
    // allow-direct-spawn: integration-test driver invoking the containment harness binary.
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
        fbuild_core::platform::process::pid_is_alive(child_pid),
        "child {} is not alive before kill",
        child_pid
    );
    assert!(
        fbuild_core::platform::process::pid_is_alive(grandchild_pid),
        "grandchild {} is not alive before kill",
        grandchild_pid
    );

    // Hard-kill the parent.
    fbuild_core::platform::process::terminate_pid(
        parent_pid,
        fbuild_core::platform::process::Termination::Force,
    )
    .expect("hard-kill parent");

    // Wait for the parent to be reaped. This is necessary on Windows
    // because the Job Object's kill-on-close only fires after the job
    // handle goes away, which requires the parent process to have fully
    // exited and its HANDLE to be closed by the test driver's `Child`.
    //
    // #806: bound the wait so a missed taskkill can't wedge the test
    // before the polling-loop deadline below has a chance to fire.
    let parent_exited = wait_with_timeout(&mut parent, Duration::from_secs(30));
    assert!(
        parent_exited,
        "parent process did not exit within 30s after kill_hard — \
         taskkill/SIGKILL failed to land"
    );

    // Poll for up to 10 s: after containment fires, both child and
    // grandchild must be gone.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let child_gone = !fbuild_core::platform::process::pid_is_alive(child_pid);
        let grand_gone = !fbuild_core::platform::process::pid_is_alive(grandchild_pid);
        if child_gone && grand_gone {
            return; // success
        }
        if Instant::now() >= deadline {
            panic!(
                "containment failed: child {} alive={}, grandchild {} alive={}",
                child_pid,
                fbuild_core::platform::process::pid_is_alive(child_pid),
                grandchild_pid,
                fbuild_core::platform::process::pid_is_alive(grandchild_pid),
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

// ---------------------------------------------------------------------------
// OS-specific PID probes and hard-kill
// ---------------------------------------------------------------------------
