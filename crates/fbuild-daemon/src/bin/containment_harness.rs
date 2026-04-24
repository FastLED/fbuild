//! Integration-test harness for process containment (FastLED/fbuild#32).
//!
//! This binary is only compiled for the `fbuild-daemon` test suite —
//! it is not shipped. It has three roles, selected by `argv[1]`:
//!
//! | role       | behaviour                                                         |
//! |------------|-------------------------------------------------------------------|
//! | `parent`   | Install the global containment group, spawn a contained `child`   |
//! |            | process, print `<parent-pid> <child-pid> <grandchild-pid>` to     |
//! |            | stdout, then sleep forever so the test can hard-kill the parent.  |
//! | `child`    | Spawn a contained `grandchild`, print its own PID and the         |
//! |            | grandchild's PID to stdout, then sleep forever.                   |
//! | `grandchild` | Sleep forever. Exists to prove grandchild-level containment.    |
//!
//! The test driver (`tests/process_containment.rs`) parses the PIDs
//! printed by each role and uses OS-specific probes to verify every PID
//! is gone after the parent is killed.

use std::io::Write;
use std::time::Duration;

fn main() {
    // Slurp the role argument; panic loudly if missing because the test
    // harness should always pass one.
    let args: Vec<String> = std::env::args().collect();
    let role = args
        .get(1)
        .map(|s| s.as_str())
        .expect("containment_harness requires a role argument (parent|child|grandchild)");

    match role {
        "parent" => run_parent(),
        "child" => run_child(),
        "grandchild" => run_grandchild(),
        other => panic!("unknown containment_harness role: {other}"),
    }
}

fn run_parent() {
    // Install the process-wide containment group. In production this is
    // done by the fbuild-daemon binary; here we reproduce the same call
    // site so the test exercises real behaviour, not a mock.
    fbuild_core::containment::init_global_containment("FBUILD-TEST")
        .expect("init_global_containment");

    // Spawn the child via the contained-spawn helper.
    let self_exe = std::env::current_exe().expect("current_exe");
    // allow-direct-spawn: integration-test harness exercising spawn_contained itself.
    let mut cmd = std::process::Command::new(&self_exe);
    cmd.arg("child")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child = fbuild_core::containment::spawn_contained(&mut cmd).expect("spawn child");

    // The child prints `<child-pid> <grandchild-pid>\n` to stdout on
    // startup. Wait for that single line before announcing our own PIDs.
    let mut stdout = child.stdout.take().expect("child stdout");
    let mut buf = Vec::<u8>::new();
    let mut byte = [0u8; 1];
    loop {
        match std::io::Read::read(&mut stdout, &mut byte) {
            Ok(0) => break,
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => buf.push(byte[0]),
            Err(_) => break,
        }
    }
    let child_line = String::from_utf8_lossy(&buf).trim().to_string();

    // Emit `<parent-pid> <child-line>\n` on stdout — this is the
    // protocol the test driver parses.
    let line = format!("{} {}\n", std::process::id(), child_line);
    std::io::stdout()
        .write_all(line.as_bytes())
        .expect("write line");
    std::io::stdout().flush().ok();

    // Drop the child handle without waiting — containment keeps it
    // alive, and we want the handle dropped so the Windows job object
    // is the only thing keeping track.
    std::mem::forget(child);

    // Sleep forever (capped at 2 minutes so a leaked process dies on
    // its own in pathological failure modes).
    std::thread::sleep(Duration::from_secs(120));
}

fn run_child() {
    // Spawn the grandchild inside the same containment group that the
    // parent installed — but we haven't installed anything here. The
    // grandchild inherits containment transparently on Unix (process
    // group membership is inherited) and on Windows (Job Objects
    // auto-include descendants because we set BREAKAWAY_OK on the job
    // but do not use `CREATE_BREAKAWAY_FROM_JOB` when spawning).
    //
    // So here we use a plain `spawn()` and the grandchild still ends
    // up in the parent's containment group.
    let self_exe = std::env::current_exe().expect("current_exe");
    // allow-direct-spawn: integration-test harness verifying grandchild containment inheritance.
    let mut cmd = std::process::Command::new(&self_exe);
    cmd.arg("grandchild")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let child = cmd.spawn().expect("spawn grandchild");
    let grandchild_pid = child.id();

    let line = format!("{} {}\n", std::process::id(), grandchild_pid);
    std::io::stdout()
        .write_all(line.as_bytes())
        .expect("write line");
    std::io::stdout().flush().ok();

    // Drop the handle and sleep forever.
    std::mem::forget(child);
    std::thread::sleep(Duration::from_secs(120));
}

fn run_grandchild() {
    std::thread::sleep(Duration::from_secs(120));
}
