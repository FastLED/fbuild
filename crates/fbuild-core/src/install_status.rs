//! Best-effort install/download status events shared across fbuild crates.
//!
//! Subscriber contract (FastLED/fbuild#807 — timeout audit):
//! the registered subscriber MUST be **non-blocking** and **fast**.
//! `publish_install_status` invokes it synchronously on the publisher's
//! thread, which is often the toolchain installer's hot path; a
//! subscriber that takes a lock, blocks on a channel send, or does I/O
//! will back-pressure every install/download progress event in the
//! workspace.
//!
//! Hardening applied here (cheap, no thread-model change):
//!   * the synchronous call is wrapped in `catch_unwind` so a panic
//!     inside the subscriber never propagates back to the publisher
//!     and never poisons the registry lock;
//!   * the contract is documented on both `set_install_status_subscriber`
//!     and `publish_install_status`.
//!
//! If a real-world subscriber ever needs to do meaningful work, it
//! should `tokio::spawn` / `mpsc::send` internally and return
//! immediately — not block here.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, OnceLock, RwLock};
use std::{fmt, fmt::Display};

use serde::{Deserialize, Serialize};

type Subscriber = Arc<dyn Fn(InstallStatus) + Send + Sync + 'static>;

static SUBSCRIBER: OnceLock<RwLock<Option<Subscriber>>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallPhase {
    WaitingForLock,
    Downloading,
    Verifying,
    Extracting,
    Installed,
    Failed,
}

impl Display for InstallPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::WaitingForLock => "waiting_for_lock",
            Self::Downloading => "downloading",
            Self::Verifying => "verifying",
            Self::Extracting => "extracting",
            Self::Installed => "installed",
            Self::Failed => "failed",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallRole {
    Installer,
    Waiter,
}

impl Display for InstallRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Installer => "installer",
            Self::Waiter => "waiter",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallStatus {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub phase: InstallPhase,
    pub role: InstallRole,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lock: Option<String>,
}

/// Register a subscriber for install/download status events.
///
/// The subscriber MUST be non-blocking and fast — see the module-level
/// doc comment. A subscriber that needs to do real work should hand
/// the status off through a channel or `tokio::spawn` and return
/// immediately.
pub fn set_install_status_subscriber<F>(subscriber: F)
where
    F: Fn(InstallStatus) + Send + Sync + 'static,
{
    let slot = SUBSCRIBER.get_or_init(|| RwLock::new(None));
    if let Ok(mut guard) = slot.write() {
        *guard = Some(Arc::new(subscriber));
    }
}

pub fn clear_install_status_subscriber() {
    if let Some(slot) = SUBSCRIBER.get() {
        if let Ok(mut guard) = slot.write() {
            *guard = None;
        }
    }
}

/// Publish an install/download status event to the registered
/// subscriber (if any). Best-effort: never returns errors, never
/// blocks beyond the subscriber's own runtime, never propagates a
/// panic out of the subscriber to the caller.
///
/// See the module-level doc for the non-blocking subscriber contract.
pub fn publish_install_status(status: InstallStatus) {
    let Some(slot) = SUBSCRIBER.get() else {
        return;
    };
    let subscriber = slot
        .read()
        .ok()
        .and_then(|guard| guard.as_ref().map(Arc::clone));
    if let Some(subscriber) = subscriber {
        // Containment: a panicking subscriber must not unwind back into
        // the publisher's thread (which is typically the toolchain
        // installer's hot path) and must not poison the SUBSCRIBER
        // lock. The closure itself sits behind `Arc<dyn Fn + Send +
        // Sync>` so `AssertUnwindSafe` is the appropriate witness —
        // there is no &mut state we could leave in a torn intermediate
        // state across the unwind boundary.
        let _ = catch_unwind(AssertUnwindSafe(move || {
            subscriber(status);
        }));
    }
}

pub fn status(
    name: impl Into<String>,
    version: Option<impl Into<String>>,
    phase: InstallPhase,
    role: InstallRole,
    message: impl Into<String>,
    lock: Option<impl Into<String>>,
) -> InstallStatus {
    InstallStatus {
        name: name.into(),
        version: version.map(Into::into),
        phase,
        role,
        message: message.into(),
        lock: lock.map(Into::into),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn subscriber_receives_published_status() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_for_callback = Arc::clone(&seen);
        set_install_status_subscriber(move |status| {
            seen_for_callback.lock().unwrap().push(status);
        });

        publish_install_status(status(
            "toolchain",
            Some("1.0"),
            InstallPhase::WaitingForLock,
            InstallRole::Waiter,
            "waiting",
            Some(".toolchain.install.lock"),
        ));

        let statuses = seen.lock().unwrap();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].name, "toolchain");
        assert_eq!(statuses[0].phase, InstallPhase::WaitingForLock);
        clear_install_status_subscriber();
    }

    #[test]
    fn panicking_subscriber_does_not_unwind_into_publisher() {
        // Regression for #807: a buggy subscriber must not be able to
        // kill the toolchain installer's hot path.
        set_install_status_subscriber(|_status| {
            panic!("subscriber blew up");
        });

        // This call must return normally — the panic is contained.
        publish_install_status(status(
            "toolchain",
            Some("1.0"),
            InstallPhase::Downloading,
            InstallRole::Installer,
            "downloading",
            None::<&str>,
        ));

        // A second publish from a re-registered subscriber must still
        // work — proving the SUBSCRIBER RwLock was not poisoned by
        // the panic.
        let seen = Arc::new(Mutex::new(0u32));
        let seen_for_cb = Arc::clone(&seen);
        set_install_status_subscriber(move |_status| {
            *seen_for_cb.lock().unwrap() += 1;
        });
        publish_install_status(status(
            "toolchain",
            Some("1.0"),
            InstallPhase::Installed,
            InstallRole::Installer,
            "done",
            None::<&str>,
        ));
        assert_eq!(*seen.lock().unwrap(), 1);
        clear_install_status_subscriber();
    }

    #[test]
    fn phase_and_role_display_match_json_names() {
        assert_eq!(InstallPhase::WaitingForLock.to_string(), "waiting_for_lock");
        assert_eq!(InstallPhase::Downloading.to_string(), "downloading");
        assert_eq!(InstallPhase::Verifying.to_string(), "verifying");
        assert_eq!(InstallPhase::Extracting.to_string(), "extracting");
        assert_eq!(InstallPhase::Installed.to_string(), "installed");
        assert_eq!(InstallPhase::Failed.to_string(), "failed");
        assert_eq!(InstallRole::Installer.to_string(), "installer");
        assert_eq!(InstallRole::Waiter.to_string(), "waiter");
    }
}
