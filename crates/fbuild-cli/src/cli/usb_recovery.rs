//! One-shot elevated USB recovery helper entry point.
//!
//! This module owns only the private JSON rendezvous used by the normal CLI
//! and its elevated copy. It never discovers or starts a daemon; PnP work is
//! delegated to the narrow `fbuild-serial` allowlist. FastLED/fbuild#1148.

use fbuild_core::usb::{UsbRecoveryPolicy, UsbRecoveryRequest, UsbRecoveryResult};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const MAX_RENDEZVOUS_BYTES: u64 = 16 * 1024;

#[derive(Debug, Deserialize, Serialize)]
pub struct RecoveryHelperEnvelope {
    pub nonce: String,
    pub request: UsbRecoveryRequest,
}

/// Facts used to enforce the CLI-side elevation policy. They are explicit so
/// tests can prove CI and non-interactive sessions never launch UAC.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecoveryLaunchContext {
    pub is_windows: bool,
    pub is_ci: bool,
    pub is_interactive: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryLaunchDecision {
    ManualGuidance,
    RefuseNonInteractive,
    LaunchOnce,
}

/// UAC launch result. Cancellation is expected user control flow, not a
/// daemon failure; the normal caller cleans the rendezvous in either case.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HelperLaunchOutcome {
    Completed { exit_code: u32 },
    Cancelled,
}

/// Test seam around the only operation that can display UAC.
pub trait RecoveryHelperLauncher {
    fn launch(
        &mut self,
        request_path: &Path,
        result_path: &Path,
    ) -> fbuild_core::Result<HelperLaunchOutcome>;
}

/// A nonce-bound pair of files owned by the normal process. Drop removes both
/// paths after helper completion/cancellation/error; neither is a daemon or
/// cache artifact.
#[derive(Debug)]
pub struct RecoveryRendezvous {
    pub request_path: PathBuf,
    pub result_path: PathBuf,
    pub nonce: String,
}

impl Drop for RecoveryRendezvous {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.request_path);
        let _ = std::fs::remove_file(&self.result_path);
    }
}

/// Per-user, short-lived rendezvous root. It is intentionally outside daemon
/// and cache directories, so the elevated helper never takes ownership of any
/// persistent fbuild state.
pub fn rendezvous_dir() -> PathBuf {
    fbuild_paths::temp_subdir("usb-recovery")
}

pub fn decide_recovery_launch(
    policy: UsbRecoveryPolicy,
    has_typed_request: bool,
    context: RecoveryLaunchContext,
) -> RecoveryLaunchDecision {
    if !has_typed_request
        || matches!(
            policy,
            UsbRecoveryPolicy::Default | UsbRecoveryPolicy::DenyAdmin
        )
    {
        return RecoveryLaunchDecision::ManualGuidance;
    }
    if !context.is_windows || context.is_ci || !context.is_interactive {
        RecoveryLaunchDecision::RefuseNonInteractive
    } else {
        RecoveryLaunchDecision::LaunchOnce
    }
}

/// Create the private payload before the normal process launches its own
/// executable with a fixed hidden helper command line.
pub fn create_rendezvous(request: UsbRecoveryRequest) -> fbuild_core::Result<RecoveryRendezvous> {
    let root = rendezvous_dir();
    let mut request_file = tempfile::Builder::new()
        .prefix("request-")
        .suffix(".json")
        .rand_bytes(32)
        .tempfile_in(&root)
        .map_err(|error| {
            fbuild_core::FbuildError::Other(format!("cannot create recovery request: {error}"))
        })?;
    let request_path = request_file.path().to_path_buf();
    let nonce = blake3::hash(request_path.to_string_lossy().as_bytes())
        .to_hex()
        .to_string();
    let envelope = RecoveryHelperEnvelope {
        nonce: nonce.clone(),
        request,
    };
    serde_json::to_writer(&mut request_file, &envelope).map_err(|error| {
        fbuild_core::FbuildError::Other(format!("cannot encode recovery request: {error}"))
    })?;
    request_file.as_file_mut().sync_all().map_err(|error| {
        fbuild_core::FbuildError::Other(format!("cannot sync recovery request: {error}"))
    })?;
    let result_path = root.join(format!("result-{nonce}.json"));
    if result_path.exists() {
        return Err(fbuild_core::FbuildError::Other(
            "recovery result path unexpectedly already exists".to_string(),
        ));
    }
    // `keep` preserves the original create-new file rather than replacing it
    // at a caller-controlled path.
    let (_file, request_path) = request_file.keep().map_err(|error| {
        fbuild_core::FbuildError::Other(format!("cannot persist recovery request: {}", error.error))
    })?;
    Ok(RecoveryRendezvous {
        request_path,
        result_path,
        nonce,
    })
}

/// Launch at most once after the policy decision was made by the normal CLI.
/// No caller can launch a helper without a typed request, and all terminal
/// paths drop the rendezvous files.
pub fn launch_once_for_typed_request<L: RecoveryHelperLauncher>(
    policy: UsbRecoveryPolicy,
    request: UsbRecoveryRequest,
    context: RecoveryLaunchContext,
    launcher: &mut L,
) -> fbuild_core::Result<RecoveryLaunchDecision> {
    let decision = decide_recovery_launch(policy, true, context);
    if decision != RecoveryLaunchDecision::LaunchOnce {
        return Ok(decision);
    }
    let rendezvous = create_rendezvous(request)?;
    let _outcome = launcher.launch(&rendezvous.request_path, &rendezvous.result_path)?;
    Ok(decision)
}

/// Windows implementation that launches only the current fbuild executable
/// with the fixed hidden helper shape. It is deliberately not a general
/// command runner and never launches a daemon.
#[cfg(windows)]
pub struct WindowsUacLauncher;

#[cfg(windows)]
impl RecoveryHelperLauncher for WindowsUacLauncher {
    fn launch(
        &mut self,
        request_path: &Path,
        result_path: &Path,
    ) -> fbuild_core::Result<HelperLaunchOutcome> {
        use windows_sys::Win32::Foundation::{
            CloseHandle, ERROR_CANCELLED, GetLastError, WAIT_OBJECT_0,
        };
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, INFINITE, WaitForSingleObject,
        };
        use windows_sys::Win32::UI::Shell::{
            SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

        fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
            use std::os::windows::ffi::OsStrExt;
            value.encode_wide().chain(Some(0)).collect()
        }
        fn quoted_path(path: &Path) -> fbuild_core::Result<String> {
            let value = path.to_string_lossy();
            if value.contains('"') || value.contains('\n') || value.contains('\r') {
                return Err(fbuild_core::FbuildError::Other(
                    "recovery helper path contains an unsupported character".to_string(),
                ));
            }
            Ok(format!("\"{value}\""))
        }

        let executable = std::env::current_exe().map_err(|error| {
            fbuild_core::FbuildError::Other(format!(
                "cannot locate current fbuild executable: {error}"
            ))
        })?;
        let parameters = format!(
            "__usb-recovery-helper --request {} --result {}",
            quoted_path(request_path)?,
            quoted_path(result_path)?
        );
        let executable_wide = wide(executable.as_os_str());
        let verb_wide = wide(std::ffi::OsStr::new("runas"));
        let parameters_wide = wide(std::ffi::OsStr::new(&parameters));
        // SAFETY: all pointer fields target NUL-terminated buffers that remain
        // alive through ShellExecuteExW; zero is the documented initialization
        // for unused structure fields.
        let mut execute: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
        execute.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        execute.fMask = SEE_MASK_NOCLOSEPROCESS;
        execute.lpVerb = verb_wide.as_ptr();
        execute.lpFile = executable_wide.as_ptr();
        execute.lpParameters = parameters_wide.as_ptr();
        execute.nShow = SW_HIDE;
        // SAFETY: `execute` is fully initialized as described above and only
        // invokes this executable with the fixed helper subcommand.
        if unsafe { ShellExecuteExW(&mut execute) } == 0 {
            // SAFETY: GetLastError reads thread-local state immediately after
            // ShellExecuteExW's documented failure return.
            let error = unsafe { GetLastError() };
            if error == ERROR_CANCELLED {
                return Ok(HelperLaunchOutcome::Cancelled);
            }
            return Err(fbuild_core::FbuildError::Other(format!(
                "UAC helper launch failed ({error})"
            )));
        }
        if execute.hProcess == 0 {
            return Err(fbuild_core::FbuildError::Other(
                "UAC helper launch returned no process handle".to_string(),
            ));
        }
        // SAFETY: hProcess was requested through SEE_MASK_NOCLOSEPROCESS and
        // belongs to this caller. The handle is closed on every path below.
        let wait = unsafe { WaitForSingleObject(execute.hProcess, INFINITE) };
        if wait != WAIT_OBJECT_0 {
            // SAFETY: closes the valid owned process handle before returning.
            unsafe { CloseHandle(execute.hProcess) };
            return Err(fbuild_core::FbuildError::Other(format!(
                "UAC helper wait failed ({wait})"
            )));
        }
        let mut exit_code = 1u32;
        // SAFETY: valid completed process handle and writable local exit code.
        let got_exit_code = unsafe { GetExitCodeProcess(execute.hProcess, &mut exit_code) };
        // SAFETY: closes the valid owned process handle exactly once.
        unsafe { CloseHandle(execute.hProcess) };
        if got_exit_code == 0 {
            return Err(fbuild_core::FbuildError::Other(
                "cannot read UAC helper exit status".to_string(),
            ));
        }
        Ok(HelperLaunchOutcome::Completed { exit_code })
    }
}

/// Execute the hidden helper after `dispatch::async_main` has routed to it
/// before tracing, environment capture, update checks, or daemon discovery.
pub fn run_hidden_helper(request_path: &Path, result_path: &Path) -> fbuild_core::Result<()> {
    validate_rendezvous_paths(request_path, result_path)?;
    let envelope = read_envelope(request_path)?;
    if !valid_nonce(&envelope.nonce) || !envelope.request.has_canonical_identity() {
        return Err(fbuild_core::FbuildError::Other(
            "recovery helper rejected malformed rendezvous payload".to_string(),
        ));
    }

    let result =
        fbuild_serial::usb_recovery::recover_windows_usb_device(&envelope.request, envelope.nonce);
    write_result_create_new(result_path, &result)
}

fn read_envelope(path: &Path) -> fbuild_core::Result<RecoveryHelperEnvelope> {
    reject_reparse_point(path, "recovery request")?;
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        fbuild_core::FbuildError::Other(format!("cannot inspect recovery request: {error}"))
    })?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(fbuild_core::FbuildError::Other(
            "recovery request must be a regular file".to_string(),
        ));
    }
    if metadata.len() > MAX_RENDEZVOUS_BYTES {
        return Err(fbuild_core::FbuildError::Other(
            "recovery request exceeds the fixed size limit".to_string(),
        ));
    }
    let mut contents = String::new();
    File::open(path)
        .and_then(|mut file| file.read_to_string(&mut contents))
        .map_err(|error| {
            fbuild_core::FbuildError::Other(format!("cannot read recovery request: {error}"))
        })?;
    serde_json::from_str(&contents).map_err(|error| {
        fbuild_core::FbuildError::Other(format!("invalid recovery request JSON: {error}"))
    })
}

fn write_result_create_new(path: &Path, result: &UsbRecoveryResult) -> fbuild_core::Result<()> {
    let encoded = serde_json::to_vec(result).map_err(|error| {
        fbuild_core::FbuildError::Other(format!("cannot encode recovery result: {error}"))
    })?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| {
            fbuild_core::FbuildError::Other(format!("cannot create recovery result: {error}"))
        })?;
    file.write_all(&encoded)
        .and_then(|()| file.sync_all())
        .map_err(|error| {
            fbuild_core::FbuildError::Other(format!("cannot write recovery result: {error}"))
        })
}

fn validate_rendezvous_paths(request_path: &Path, result_path: &Path) -> fbuild_core::Result<()> {
    let root = rendezvous_dir();
    reject_reparse_point(&root, "recovery rendezvous directory")?;
    let request_parent = request_path.parent();
    let result_parent = result_path.parent();
    let valid_parent =
        request_parent == Some(root.as_path()) && result_parent == Some(root.as_path());
    let request_name = request_path.file_name().and_then(|name| name.to_str());
    let result_name = result_path.file_name().and_then(|name| name.to_str());
    if !valid_parent
        || !request_name.is_some_and(|name| name.starts_with("request-") && name.ends_with(".json"))
        || !result_name.is_some_and(|name| name.starts_with("result-") && name.ends_with(".json"))
    {
        return Err(fbuild_core::FbuildError::Other(
            "recovery helper rejected paths outside its private rendezvous directory".to_string(),
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn reject_reparse_point(path: &Path, description: &str) -> fbuild_core::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, GetFileAttributesW, INVALID_FILE_ATTRIBUTES,
    };

    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    // SAFETY: `wide` is NUL-terminated and remains live for the API call.
    let attributes = unsafe { GetFileAttributesW(wide.as_ptr()) };
    if attributes == INVALID_FILE_ATTRIBUTES {
        return Err(fbuild_core::FbuildError::Other(format!(
            "cannot inspect {description} attributes"
        )));
    }
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(fbuild_core::FbuildError::Other(format!(
            "recovery helper rejected reparse-point {description}"
        )));
    }
    Ok(())
}

#[cfg(not(windows))]
fn reject_reparse_point(_path: &Path, _description: &str) -> fbuild_core::Result<()> {
    Ok(())
}

fn valid_nonce(nonce: &str) -> bool {
    (32..=128).contains(&nonce.len()) && nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fbuild_core::usb::UsbRecoveryHealth;

    fn envelope() -> RecoveryHelperEnvelope {
        RecoveryHelperEnvelope {
            nonce: "0123456789abcdef0123456789abcdef".to_string(),
            request: UsbRecoveryRequest {
                operation_id: "deploy-1".to_string(),
                instance_id: "USB\\VID_2E8A&PID_000A\\serial".to_string(),
                expected_class: "Ports".to_string(),
                parent_instance_id: Some("USB\\ROOT_HUB30\\parent".to_string()),
                expected_vid: 0x2e8a,
                expected_pid: 0x000a,
                expected_serial: Some("serial".to_string()),
                problem_code: Some(43),
                flash_completed: true,
            },
        }
    }

    #[test]
    fn nonce_must_be_bounded_hex() {
        assert!(valid_nonce(&envelope().nonce));
        assert!(!valid_nonce("too-short"));
        assert!(!valid_nonce("0123456789abcdef0123456789abcdef!"));
    }

    #[test]
    fn result_file_is_create_new() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("result-test.json");
        let result = UsbRecoveryResult {
            operation_id: "deploy-1".to_string(),
            nonce: envelope().nonce,
            validated_instance_id: None,
            operation: None,
            before: UsbRecoveryHealth::Unknown,
            after: UsbRecoveryHealth::Unknown,
            success: false,
            error_code: Some("test".to_string()),
        };
        assert!(write_result_create_new(&path, &result).is_ok());
        assert!(write_result_create_new(&path, &result).is_err());
    }

    #[test]
    fn default_no_admin_ci_and_noninteractive_never_launch() {
        let interactive_windows = RecoveryLaunchContext {
            is_windows: true,
            is_ci: false,
            is_interactive: true,
        };
        assert_eq!(
            decide_recovery_launch(UsbRecoveryPolicy::Default, true, interactive_windows),
            RecoveryLaunchDecision::ManualGuidance
        );
        assert_eq!(
            decide_recovery_launch(UsbRecoveryPolicy::DenyAdmin, true, interactive_windows),
            RecoveryLaunchDecision::ManualGuidance
        );
        assert_eq!(
            decide_recovery_launch(
                UsbRecoveryPolicy::AllowAdmin,
                true,
                RecoveryLaunchContext {
                    is_ci: true,
                    ..interactive_windows
                },
            ),
            RecoveryLaunchDecision::RefuseNonInteractive
        );
        assert_eq!(
            decide_recovery_launch(
                UsbRecoveryPolicy::AllowAdmin,
                true,
                RecoveryLaunchContext {
                    is_interactive: false,
                    ..interactive_windows
                },
            ),
            RecoveryLaunchDecision::RefuseNonInteractive
        );
    }

    #[test]
    fn admin_launches_once_only_for_a_typed_request() {
        struct FakeLauncher {
            calls: usize,
            request_path: Option<PathBuf>,
            result_path: Option<PathBuf>,
        }
        impl RecoveryHelperLauncher for FakeLauncher {
            fn launch(
                &mut self,
                request_path: &Path,
                result_path: &Path,
            ) -> fbuild_core::Result<HelperLaunchOutcome> {
                self.calls += 1;
                self.request_path = Some(request_path.to_path_buf());
                self.result_path = Some(result_path.to_path_buf());
                Ok(HelperLaunchOutcome::Cancelled)
            }
        }

        let context = RecoveryLaunchContext {
            is_windows: true,
            is_ci: false,
            is_interactive: true,
        };
        let mut launcher = FakeLauncher {
            calls: 0,
            request_path: None,
            result_path: None,
        };
        assert_eq!(
            decide_recovery_launch(UsbRecoveryPolicy::AllowAdmin, false, context),
            RecoveryLaunchDecision::ManualGuidance
        );
        let decision = launch_once_for_typed_request(
            UsbRecoveryPolicy::AllowAdmin,
            envelope().request,
            context,
            &mut launcher,
        )
        .expect("launch policy");
        assert_eq!(decision, RecoveryLaunchDecision::LaunchOnce);
        assert_eq!(launcher.calls, 1);
        assert!(!launcher.request_path.expect("request path").exists());
        assert!(!launcher.result_path.expect("result path").exists());
    }
}
