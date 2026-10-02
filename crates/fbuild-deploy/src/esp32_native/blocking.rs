//! Wall-clock cap for port-owning blocking work (FastLED/fbuild#1614).
//!
//! espflash's `Flasher` is synchronous, so the native verify/write paths
//! run it on `spawn_blocking` under a `tokio::time::timeout`. A timeout
//! only drops the `JoinHandle`: a blocking closure cannot be cancelled,
//! so the thread keeps running — and keeps the serial port open — until
//! espflash gives up on its own. Returning at that point hands the
//! caller's esptool fallback a port that is still held, and it fails
//! with `EBUSY` ("the port is busy or doesn't exist").
//!
//! [`run_port_bound`] keeps joining the thread for a bounded release
//! grace after the budget expires, so a timeout error means the port has
//! actually been released. If even the grace expires, the error says the
//! port is still held instead of letting the fallback misreport it.

use std::time::Duration;

use fbuild_core::{FbuildError, Result};

/// How long to keep waiting for a timed-out native operation to release
/// its serial port. espflash bounds every UART read/write (3 s for
/// verify, 10 s for write), so an abandoned connect/sync loop unwinds on
/// its own; on an ESP32-C6 USB-Serial-JTAG bench it took ~40 s past the
/// 30 s verify budget.
pub(crate) const PORT_RELEASE_GRACE: Duration = Duration::from_secs(60);

/// Run `op` on a blocking thread with a `budget` wall-clock cap. On
/// timeout, wait up to `release_grace` more for the thread to finish so
/// the serial port it owns is closed before returning.
pub(crate) async fn run_port_bound<T, F>(
    what: &str,
    port: &str,
    budget: Duration,
    release_grace: Duration,
    op: F,
) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    let mut handle = tokio::task::spawn_blocking(op);
    let joined = match tokio::time::timeout(budget, &mut handle).await {
        Ok(joined) => joined,
        Err(_) => {
            tracing::warn!(
                port,
                "{what} timed out after {}s; waiting up to {}s for it to release the port",
                budget.as_secs(),
                release_grace.as_secs()
            );
            let released = tokio::time::timeout(release_grace, handle).await.is_ok();
            return Err(FbuildError::DeployFailed(if released {
                format!("{what} timed out after {}s on {port}", budget.as_secs())
            } else {
                format!(
                    "{what} timed out after {}s on {port}, and {port} is still held by \
                     the abandoned operation {}s later",
                    budget.as_secs(),
                    release_grace.as_secs()
                )
            }));
        }
    };
    joined.unwrap_or_else(|e| {
        Err(FbuildError::DeployFailed(format!(
            "{what}: blocking task panicked: {e}"
        )))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[tokio::test]
    async fn returns_result_within_budget() {
        let out = run_port_bound("op", "p", Duration::from_secs(5), Duration::ZERO, || Ok(7))
            .await
            .unwrap();
        assert_eq!(out, 7);
    }

    /// The bug: a timeout must not return while the thread (which owns
    /// the port) is still running.
    #[tokio::test]
    async fn timeout_waits_for_the_thread_to_finish() {
        let finished = Arc::new(AtomicBool::new(false));
        let flag = finished.clone();
        let err = run_port_bound(
            "native verify",
            "/dev/ttyACM1",
            Duration::from_millis(50),
            Duration::from_secs(5),
            move || {
                std::thread::sleep(Duration::from_millis(300));
                flag.store(true, Ordering::SeqCst);
                Ok(())
            },
        )
        .await
        .unwrap_err();
        assert!(
            finished.load(Ordering::SeqCst),
            "returned before the port was released"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("native verify timed out after 0s on /dev/ttyACM1"),
            "{msg}"
        );
        assert!(!msg.contains("still held"), "{msg}");
    }

    #[tokio::test]
    async fn grace_expiry_reports_the_port_as_still_held() {
        let err = run_port_bound(
            "native write",
            "/dev/ttyACM1",
            Duration::from_millis(20),
            Duration::from_millis(20),
            || {
                std::thread::sleep(Duration::from_millis(500));
                Ok(())
            },
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("/dev/ttyACM1 is still held"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn panic_is_reported() {
        let err =
            run_port_bound::<(), _>("op", "p", Duration::from_secs(5), Duration::ZERO, || {
                panic!("boom")
            })
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("op: blocking task panicked"),
            "{err}"
        );
    }
}
