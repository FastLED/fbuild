//! Best-effort install/download status events shared across fbuild crates.

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

pub fn publish_install_status(status: InstallStatus) {
    let Some(slot) = SUBSCRIBER.get() else {
        return;
    };
    let subscriber = slot
        .read()
        .ok()
        .and_then(|guard| guard.as_ref().map(Arc::clone));
    if let Some(subscriber) = subscriber {
        subscriber(status);
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
