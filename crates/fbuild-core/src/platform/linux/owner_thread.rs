//! One process-lifetime thread that forks every contained Tokio child on Linux
//! (FastLED/fbuild#1628).
//!
//! Linux owner-death is `PR_SET_PDEATHSIG`, and the kernel delivers it when the
//! *thread* that forked the child exits, not the process. Forking from whatever
//! Tokio thread the caller happens to run on tied a compiler's life to that
//! thread: a worker demoted by `block_in_place` retires from the blocking pool
//! after its idle keep-alive and SIGKILLs every child it forked, mid-compile.
//! Forking here instead makes the signal mean what containment intends -- the
//! daemon process died -- because this thread lives as long as the process.

use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel};

type SpawnFn = fn(&mut tokio::process::Command) -> std::io::Result<tokio::process::Child>;
type Reply = (
    tokio::process::Command,
    std::io::Result<tokio::process::Child>,
);
struct Job {
    command: tokio::process::Command,
    runtime: tokio::runtime::Handle,
    spawn: SpawnFn,
    reply: Sender<Reply>,
}

static JOBS: OnceLock<Sender<Job>> = OnceLock::new();

fn jobs() -> std::io::Result<&'static Sender<Job>> {
    if let Some(jobs) = JOBS.get() {
        return Ok(jobs);
    }
    let (sender, receiver) = channel::<Job>();
    std::thread::Builder::new()
        .name("fbuild-child-owner".to_string())
        .spawn(move || {
            // Never returns while the process lives: `JOBS` holds a sender.
            while let Ok(mut job) = receiver.recv() {
                let _entered = job.runtime.enter();
                // A panicking spawn fails its own caller, not every later one.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    (job.spawn)(&mut job.command)
                }))
                .unwrap_or_else(|_| Err(std::io::Error::other("child spawn panicked")));
                let _ = job.reply.send((job.command, result));
            }
        })?;
    // A racing initializer may win; its thread serves every later spawn and
    // this one's thread exits once its receiver sees the sender dropped.
    Ok(JOBS.get_or_init(|| sender))
}

/// Run `spawn` for `command` on the owner thread, under the caller's Tokio
/// runtime so the child is reaped by that runtime as before.
pub(crate) fn spawn_tokio_on_owner_thread(
    command: &mut tokio::process::Command,
    spawn: SpawnFn,
) -> std::io::Result<tokio::process::Child> {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        // No runtime to reap the child: Tokio's own spawn reports that.
        return spawn(command);
    };
    let (reply, response) = channel();
    let job = Job {
        // allow-direct-spawn: inert placeholder held while the real command is on the owner thread; never spawned
        command: std::mem::replace(command, tokio::process::Command::new("")),
        runtime,
        spawn,
        reply,
    };
    jobs()?
        .send(job)
        .map_err(|_| std::io::Error::other("child owner thread is gone"))?;
    let (returned, result) = response
        .recv()
        .map_err(|_| std::io::Error::other("child owner thread is gone"))?;
    *command = returned;
    result
}
