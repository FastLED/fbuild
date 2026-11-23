//! Windows long-path repair and interactive preflight (#1673).

use std::io::IsTerminal;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fbuild_core::platform::{fs, host, process};
use fbuild_core::{FbuildError, Result};
use fbuild_paths::long_paths::{self, Observation};

use super::args::{Cli, Commands};
use crate::{daemon_client::DaemonClient, output};

pub(crate) fn should_prompt(observation: &Observation, interactive: bool, ci: bool) -> bool {
    matches!(observation, Observation::Disabled) && interactive && !ci
}

pub(crate) fn consented(answer: &str) -> bool {
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

pub(crate) fn setting_changed(startup: Option<&Observation>, current: &Observation) -> bool {
    !matches!(current, Observation::NotApplicable)
        && startup.is_none_or(|startup| startup.status_key() != current.status_key())
}

fn ci_session() -> bool {
    [
        "CI",
        "GITHUB_ACTIONS",
        "GITLAB_CI",
        "CIRCLECI",
        "TF_BUILD",
        "JENKINS_URL",
    ]
    .iter()
    .any(|key| {
        std::env::var(key).is_ok_and(|value| {
            !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
        })
    })
}

fn is_build_operation(cli: &Cli) -> bool {
    if (cli.command.is_none() && cli.platformio)
        || matches!(
            &cli.command,
            Some(
                Commands::Build {
                    platformio: true,
                    ..
                } | Commands::Deploy {
                    platformio: true,
                    ..
                }
            )
        )
    {
        return false;
    }
    matches!(
        cli.command,
        None | Some(
            Commands::Build { .. }
                | Commands::Deploy { .. }
                | Commands::TestEmu { .. }
                | Commands::Install { .. }
                | Commands::CompileMany { .. }
                | Commands::Ci { .. }
        )
    )
}

/// The hidden helper runs before tracing, cache discovery, or daemon acquisition.
pub fn run_helper() -> Result<()> {
    fs::enable_long_paths().map_err(|error| {
        FbuildError::Other(format!("could not enable Windows long paths: {error}"))
    })
}

/// Explicit repair authorization: elevate only this executable's fixed helper.
async fn repair() -> Result<bool> {
    if !host::is_windows() {
        return Err(FbuildError::Other(
            "fbuild --fix-win is available only on Windows".into(),
        ));
    }
    if matches!(long_paths::probe(), Observation::Enabled) {
        return Ok(true);
    }
    let outcome = tokio::task::spawn_blocking(|| {
        let executable = fbuild_core::platform::executable::current_image()?;
        process::launch_elevated(executable.as_os_str(), "--__fix-win-helper")
    })
    .await
    .map_err(|error| FbuildError::Other(format!("Windows repair worker failed: {error}")))?
    .map_err(|error| {
        FbuildError::Other(format!(
            "Windows repair could not request administrator approval: {error}"
        ))
    })?;
    verify_repair(outcome, long_paths::probe())
}

fn verify_repair(outcome: process::ElevationOutcome, observation: Observation) -> Result<bool> {
    match outcome {
        process::ElevationOutcome::Declined => Ok(false),
        process::ElevationOutcome::Completed(0) if matches!(observation, Observation::Enabled) => {
            Ok(true)
        }
        process::ElevationOutcome::Completed(code) => Err(FbuildError::Other(format!(
            "Windows long-path repair was not verified (helper exit {code}). Administrator policy may prevent the change."
        ))),
    }
}

pub async fn run_fix_win() -> Result<()> {
    if !repair().await? {
        return Err(FbuildError::Other(
            "Windows repair cancelled; no setting was changed".into(),
        ));
    }
    // Retire a daemon that observed the old setting. The parent does not build.
    reconcile_daemon_owner().await?;
    output::result("Windows long paths are enabled. The next build will use fresh processes.");
    Ok(())
}

const SNOOZE_DURATION: Duration = Duration::from_secs(7 * 24 * 60 * 60);

async fn snoozed(path: &Path, now: Duration) -> bool {
    let Ok(data) = fbuild_core::fs::read(path).await else {
        return false;
    };
    let Ok(dismissed_at) = serde_json::from_slice::<Duration>(&data) else {
        return false;
    };
    now.checked_sub(dismissed_at)
        .is_some_and(|elapsed| elapsed < SNOOZE_DURATION)
}

async fn save_snooze(path: &Path, now: Duration) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fbuild_core::fs::create_dir_all(parent).await?;
    }
    fbuild_core::fs::write_atomic(path, serde_json::to_vec(&now)?).await
}

fn startup_offer(cli: &Cli, observation: &Observation, interactive: bool, ci: bool) -> bool {
    !cli.fix_win && !cli.fix_win_helper && should_prompt(observation, interactive, ci)
}

async fn offer_repair() -> Result<Option<i32>> {
    let path = long_paths::repair_snooze_path();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    if let Some(path) = &path {
        if snoozed(path.as_path(), now).await {
            return Ok(None);
        }
    }

    if output::confirmation_prompt(
        "Enable Windows long paths now? This changes a system setting and requires administrator approval.",
    )? {
        match repair().await {
            Ok(true) => {
                reconcile_daemon_owner().await?;
                return relaunch_command().await.map(Some);
            }
            Ok(false) => output::visible_warning("Administrator approval was cancelled."),
            Err(error) => {
                output::visible_warning(error);
                return Ok(None);
            }
        }
    }

    let dismissed_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    if let Some(path) = path {
        if let Err(error) = save_snooze(path.as_path(), dismissed_at).await {
            tracing::warn!(%error, "could not save Windows long-path repair snooze");
        }
    }
    Ok(None)
}

/// Returns the exit status of a freshly relaunched pending command after repair.
pub async fn prepare(cli: &Cli) -> Result<Option<i32>> {
    if !host::is_windows() {
        return Ok(None);
    }
    let observation = long_paths::probe();
    let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    if startup_offer(cli, &observation, interactive, ci_session()) {
        if let Some(code) = offer_repair().await? {
            return Ok(Some(code));
        }
    }

    if is_build_operation(cli) {
        tokio::task::spawn_blocking(long_paths::initialize)
            .await
            .map_err(|error| {
                FbuildError::Other(format!("path selection worker failed: {error}"))
            })??;
        if let Some(advisory) = long_paths::advisory() {
            output::visible_warning(advisory);
        }
    }
    Ok(None)
}

async fn relaunch_command() -> Result<i32> {
    tokio::task::spawn_blocking(|| {
        let executable = fbuild_core::platform::executable::current_image()?;
        let mut command = process::command(executable.as_path());
        command.args(std::env::args_os().skip(1));
        let mut child = process::spawn_contained_in_terminal(&mut command)?;
        child.wait()
    })
    .await
    .map_err(|error| FbuildError::Other(format!("fresh command worker failed: {error}")))?
    .map_err(|error| FbuildError::Other(format!("could not restart the pending command: {error}")))
}

/// A single mode owns the daemon/compiler store. Drain that owner before a
/// different cache selection or registry observation can start a replacement.
pub async fn reconcile_daemon_owner() -> Result<()> {
    if !host::is_windows() {
        return Ok(());
    }
    if fbuild_paths::daemon_ownership::read_owner_claim().is_none() {
        return Ok(());
    }
    const HANDOFF_WAIT: std::time::Duration = std::time::Duration::from_secs(15 * 60);
    let deadline = tokio::time::Instant::now() + HANDOFF_WAIT;
    // Serialize replacement with the existing spawn election, without breaking
    // ownership locks or forcing termination of a compiler in flight.
    let _spawn = loop {
        if let Some(guard) = fbuild_paths::daemon_ownership::try_acquire_spawn_lock_result_at(
            &fbuild_paths::daemon_ownership::spawn_lock_path(),
        )? {
            break guard;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(FbuildError::DaemonError("another invocation is still changing Windows path settings; retry after it finishes".into()));
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };
    // Read the claim and negotiated identity only AFTER election. A concurrent
    // caller may have replaced the owner while we waited for this lock.
    let Some(claim) = fbuild_paths::daemon_ownership::read_owner_claim() else {
        return Ok(());
    };
    if !process::pid_exe_stem_matches(claim.pid, fbuild_paths::daemon_ownership::DAEMON_EXE_STEM) {
        return Ok(());
    }
    let client = DaemonClient::with_port(claim.port);
    let info = match client.daemon_info().await {
        Ok(info) => info,
        Err(_)
            if !process::pid_exe_stem_matches(
                claim.pid,
                fbuild_paths::daemon_ownership::DAEMON_EXE_STEM,
            ) =>
        {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    if info.pid != claim.pid {
        return Err(FbuildError::DaemonError(
            "daemon owner identity changed during path-policy negotiation; retry the command"
                .into(),
        ));
    }
    let observation = long_paths::probe();
    let cache_changed = long_paths::current_resolution().is_some()
        && crate::daemon_client::daemon_cache_identity_error(&info).is_some();
    if !cache_changed && !setting_changed(info.long_path_support.as_ref(), &observation) {
        return Ok(());
    }
    output::visible_warning(
        "Windows path settings changed. Waiting for active work to finish before restarting the daemon.",
    );
    if let Err(error) = client.shutdown_gracefully().await {
        if process::pid_exe_stem_matches(claim.pid, fbuild_paths::daemon_ownership::DAEMON_EXE_STEM)
        {
            return Err(error);
        }
    }
    while process::pid_exe_stem_matches(claim.pid, fbuild_paths::daemon_ownership::DAEMON_EXE_STEM)
    {
        if tokio::time::Instant::now() >= deadline {
            return Err(FbuildError::DaemonError("path settings have changed, but active daemon work is still draining. It has not been interrupted; retry the command after it finishes".into()));
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fbuild_paths::long_paths::Observation;

    #[test]
    fn repair_prompt_is_interactive_and_never_runs_in_ci() {
        assert!(should_prompt(&Observation::Disabled, true, false));
        assert!(!should_prompt(
            &Observation::Unknown("access denied".into()),
            true,
            false
        ));
        assert!(!should_prompt(&Observation::Enabled, true, false));
        assert!(!should_prompt(&Observation::NotApplicable, true, false));
        assert!(!should_prompt(&Observation::Disabled, false, false));
        assert!(!should_prompt(&Observation::Disabled, true, true));
    }

    #[test]
    fn startup_offers_repair_for_device_list() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["fbuild", "device", "list"]).unwrap();
        assert!(startup_offer(&cli, &Observation::Disabled, true, false));
        let explicit = Cli::try_parse_from(["fbuild", "--fix-win"]).unwrap();
        assert!(!startup_offer(
            &explicit,
            &Observation::Disabled,
            true,
            false
        ));
    }

    #[tokio::test]
    async fn startup_snooze_survives_fresh_reads_and_expires_after_seven_days() {
        use std::time::Duration;

        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("preferences").join("long-path-snooze");
        let dismissed = Duration::from_secs(1_000_000);
        let seven_days = Duration::from_secs(7 * 24 * 60 * 60);
        assert!(!snoozed(&path, dismissed).await);
        save_snooze(&path, dismissed).await.unwrap();
        assert!(snoozed(&path, dismissed).await);
        assert!(snoozed(&path, dismissed + seven_days - Duration::from_nanos(1)).await);
        assert!(!snoozed(&path, dismissed + seven_days).await);
    }

    #[test]
    fn affirmative_consent_is_required_before_elevation() {
        for answer in ["y", "Y\n", "yes", " YES "] {
            assert!(consented(answer));
        }
        for answer in ["", "\n", "n", "no", "maybe"] {
            assert!(!consented(answer));
        }
    }

    #[test]
    fn platformio_passthrough_does_not_select_fbuild_paths() {
        use clap::Parser;
        for args in [
            vec!["fbuild", "--platformio"],
            vec!["fbuild", "build", "--platformio"],
            vec!["fbuild", "deploy", "--platformio"],
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            assert!(!is_build_operation(&cli));
        }
        assert!(is_build_operation(
            &Cli::try_parse_from(["fbuild", "build"]).unwrap()
        ));
        assert!(is_build_operation(
            &Cli::try_parse_from(["fbuild", "--platformio", "build"]).unwrap()
        ));
    }

    #[test]
    fn changing_registry_state_requires_fresh_processes() {
        assert!(setting_changed(
            Some(&Observation::Disabled),
            &Observation::Enabled
        ));
        assert!(setting_changed(
            Some(&Observation::Enabled),
            &Observation::Disabled
        ));
        assert!(!setting_changed(
            Some(&Observation::Disabled),
            &Observation::Disabled
        ));
        assert!(!setting_changed(
            Some(&Observation::Unknown("old".into())),
            &Observation::Unknown("new".into())
        ));
        assert!(!setting_changed(None, &Observation::NotApplicable));
    }

    #[test]
    fn repair_requires_successful_helper_and_registry_readback() {
        assert!(
            !verify_repair(process::ElevationOutcome::Declined, Observation::Disabled).unwrap()
        );
        assert!(
            verify_repair(
                process::ElevationOutcome::Completed(0),
                Observation::Enabled
            )
            .unwrap()
        );
        assert!(
            verify_repair(
                process::ElevationOutcome::Completed(1),
                Observation::Enabled
            )
            .is_err()
        );
        assert!(
            verify_repair(
                process::ElevationOutcome::Completed(0),
                Observation::Disabled
            )
            .is_err()
        );
        assert!(
            verify_repair(
                process::ElevationOutcome::Completed(0),
                Observation::Unknown("policy".into())
            )
            .is_err()
        );
    }
}
