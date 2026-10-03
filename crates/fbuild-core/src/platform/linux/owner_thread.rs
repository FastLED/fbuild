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

use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};

use crate::channel::{UnboundedSender, unbounded};

type SpawnFn = fn(&mut tokio::process::Command) -> std::io::Result<tokio::process::Child>;
type Reply = (
    tokio::process::Command,
    std::io::Result<tokio::process::Child>,
);

/// Where the owner thread leaves one spawn's result. The caller waits on it
/// synchronously -- `spawn_tokio_contained` is a sync fn that already forks
/// inline, and a fork is microseconds -- so this is a condvar rendezvous
/// rather than an async channel, whose blocking receive panics on a runtime
/// thread.
#[derive(Default)]
struct ReplySlot {
    value: Mutex<Option<Reply>>,
    ready: Condvar,
}

impl ReplySlot {
    fn fill(&self, reply: Reply) {
        *self.value.lock().unwrap_or_else(PoisonError::into_inner) = Some(reply);
        self.ready.notify_one();
    }

    fn take(&self) -> Reply {
        let mut value = self.value.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(reply) = value.take() {
                return reply;
            }
            value = self
                .ready
                .wait(value)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

struct Job {
    command: tokio::process::Command,
    runtime: tokio::runtime::Handle,
    spawn: SpawnFn,
    reply: Arc<ReplySlot>,
}

static JOBS: OnceLock<UnboundedSender<Job>> = OnceLock::new();

fn jobs() -> std::io::Result<&'static UnboundedSender<Job>> {
    if let Some(jobs) = JOBS.get() {
        return Ok(jobs);
    }
    let (sender, mut receiver) = unbounded::<Job>();
    std::thread::Builder::new()
        .name("fbuild-child-owner".to_string())
        .spawn(move || {
            // Never returns while the process lives: `JOBS` holds a sender.
            // The receive happens outside any runtime context; each spawn
            // enters the caller's runtime only for its own duration.
            while let Some(mut job) = receiver.blocking_recv() {
                let result = {
                    let _entered = job.runtime.enter();
                    // A panicking spawn fails its own caller, not every later one.
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        (job.spawn)(&mut job.command)
                    }))
                    .unwrap_or_else(|_| Err(std::io::Error::other("child spawn panicked")))
                };
                job.reply.fill((job.command, result));
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
        // Tokio's own spawn panics here: there is no runtime to reap the child.
        return Err(std::io::Error::other(
            "spawn_tokio_contained requires a Tokio runtime to reap the child",
        ));
    };
    let reply = Arc::new(ReplySlot::default());
    let job = Job {
        // allow-direct-spawn: inert placeholder held while the real command is on the owner thread; never spawned
        command: std::mem::replace(command, tokio::process::Command::new("")),
        runtime,
        spawn,
        reply: Arc::clone(&reply),
    };
    if let Err(unsent) = jobs()?.send(job) {
        // The owner thread is gone; hand the command back untouched.
        *command = unsent.0.command;
        return Err(std::io::Error::other("child owner thread is gone"));
    }
    let (returned, result) = reply.take();
    *command = returned;
    result
}
