//! Windows long-path repair and interactive preflight (#1673).

use std::io::IsTerminal;
use std::path::Path;

use fbuild_core::platform::{fs, host, process};
use fbuild_core::{FbuildError, Result};
use fbuild_paths::long_paths::{self, Observation};

use super::args::{Cli, Commands};
use crate::{daemon_client::DaemonClient, output};

pub(crate) fn should_prompt(observation: &Observation, interactive: bool, ci: bool) -> bool {
    observation.requires_fallback() && interactive && !ci
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

fn is_esp32_operation(cli: &Cli) -> bool {
    let (project, environments): (&str, Vec<&str>) = match &cli.command {
        Some(
            Commands::Build {
                project_dir,
                environment,
                ..
            }
            | Commands::Deploy {
                project_dir,
                environment,
                ..
            }
            | Commands::TestEmu {
                project_dir,
                environment,
                ..
            },
        ) => (
            project_dir
                .as_deref()
                .or(cli.project_dir.as_deref())
                .unwrap_or("."),
            environment.iter().map(String::as_str).collect(),
        ),
        Some(Commands::Install {
            project_dir,
            environments,
            ..
        }) => (
            project_dir
                .as_deref()
                .or(cli.project_dir.as_deref())
                .unwrap_or("."),
            environments.iter().map(String::as_str).collect(),
        ),
        Some(Commands::CompileMany { board, .. } | Commands::Ci { board, .. }) => {
            return fbuild_config::BoardConfig::from_board_id(
                board,
                &std::collections::HashMap::new(),
            )
            .is_ok_and(|board| board.platform() == Some(fbuild_core::Platform::Espressif32));
        }
        None => (
            cli.project_dir.as_deref().unwrap_or("."),
            cli.environment.iter().map(String::as_str).collect(),
        ),
        _ => return false,
    };
    let install = matches!(&cli.command, Some(Commands::Install { .. }));
    let config_path = if install {
        Path::new(project).join("platformio.ini")
    } else {
        std::env::var_os("PLATFORMIO_PROJECT_CONFIG")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| Path::new(project).join("platformio.ini"))
    };
    let Ok(config) = fbuild_config::PlatformIOConfig::from_path_with_overrides(
        &config_path,
        if install {
            fbuild_config::pio_env::PioEnvOverrides::empty()
        } else {
            fbuild_config::pio_env::PioEnvOverrides::from_map(
                crate::daemon_client::capture_pio_env(),
            )
        },
    ) else {
        return false; // The ordinary handler reports configuration errors.
    };
    selected_config_requires_esp32(
        &config,
        &environments,
        matches!(&cli.command, Some(Commands::Install { all_envs: true, .. })),
    )
}

fn selected_config_requires_esp32(
    config: &fbuild_config::PlatformIOConfig,
    environments: &[&str],
    all_envs: bool,
) -> bool {
    let environments = if all_envs {
        config.get_environments()
    } else if environments.is_empty() {
        config.get_default_environment().into_iter().collect()
    } else {
        environments.to_vec()
    };
    environments.iter().any(|env| {
        config
            .get_env_config(env)
            .ok()
            .and_then(|config| config.get("platform").cloned())
            .and_then(|platform| fbuild_core::Platform::from_platform_str(&platform))
            == Some(fbuild_core::Platform::Espressif32)
    })
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

/// Returns the exit status of a freshly relaunched pending command after repair.
pub async fn prepare(cli: &Cli) -> Result<Option<i32>> {
    if !host::is_windows() || !is_build_operation(cli) {
        return Ok(None);
    }
    let observation = long_paths::probe();
    let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    if is_build_operation(cli)
        && is_esp32_operation(cli)
        && should_prompt(&observation, interactive, ci_session())
    {
        output::visible_warning(
            "Windows long-path support is disabled or unavailable. ESP32 package paths may exceed Windows limits. Run fbuild --fix-win to repair it.",
        );
        if output::confirmation_prompt(
            "Enable Windows long paths now? This changes a system setting and requires administrator approval.",
        )? {
            match repair().await {
                Ok(true) => {
                    reconcile_daemon_owner().await?;
                    return relaunch_command().await.map(Some);
                }
                Ok(false) => output::visible_warning(
                    "Administrator approval was cancelled; selecting a short-path fallback.",
                ),
                Err(error) => {
                    output::visible_warning(format!("{error} Selecting a short-path fallback."))
                }
            }
        }
    }
    tokio::task::spawn_blocking(long_paths::initialize)
        .await
        .map_err(|error| FbuildError::Other(format!("path selection worker failed: {error}")))??;
    if is_build_operation(cli) {
        if let Some(advisory) = long_paths::advisory() {
            output::visible_warning(advisory);
        }
    }
    Ok(None)
}

async fn relaunch_command() -> Result<i32> {
    tokio::task::spawn_blocking(|| {
        let executable = fbuild_core::platform::executable::current_image()?;
        let mut command = std::process::Command::new(executable.as_path());
        command.args(std::env::args_os().skip(1));
        let mut child = process::spawn_contained(
            &mut command,
            process::ContainedStdio {
                stdin: process::StdioSource::Parent,
                stdout: process::StdioSource::Parent,
                stderr: process::StdioSource::Parent,
            },
        )?;
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
        assert!(should_prompt(
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
    fn prompt_selection_follows_requested_environment() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("platformio.ini");
        std::fs::write(&path,
            "[platformio]\ndefault_envs = uno\n[env:uno]\nplatform = atmelavr\nboard = uno\n[env:esp]\nplatform = espressif32\nboard = esp32dev\n",
        ).unwrap();
        let config = fbuild_config::PlatformIOConfig::from_path(&path).unwrap();
        assert!(!selected_config_requires_esp32(&config, &[], false));
        assert!(selected_config_requires_esp32(&config, &[], true));
        assert!(selected_config_requires_esp32(&config, &["esp"], false));
        assert!(!selected_config_requires_esp32(&config, &["uno"], false));
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
