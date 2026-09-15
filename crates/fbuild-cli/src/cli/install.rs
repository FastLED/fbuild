//! `fbuild install`: provision an environment's packages without compiling
//! (FastLED/fbuild#1433).
//!
//! Runs in-process rather than through the daemon: it only touches the package
//! cache, and CI runs it as its own step before any build, so a daemon
//! round-trip (and the compile backend's startup) would only add time.

use std::path::Path;

use fbuild_build::provision::{
    ProvisionMode, ProvisionReport, ProvisionStatus, ProvisionedPackage, packages_hash,
};
use fbuild_core::{FbuildError, Result};

use super::cache::human_bytes;
use crate::output;

/// Exit code when `--check` finds a package that would need fetching.
pub const CHECK_NEEDS_FETCH_EXIT: i32 = 2;

/// Parsed `fbuild install` arguments.
pub struct InstallArgs {
    pub project_dir: String,
    pub environments: Vec<String>,
    pub all_envs: bool,
    pub check: bool,
    pub dry_run: bool,
    pub json: bool,
    pub jobs: Option<usize>,
}

pub async fn run_install(args: InstallArgs) -> Result<()> {
    let project_dir = Path::new(&args.project_dir);
    let envs = select_envs(project_dir, &args)?;
    let mode = if args.check {
        ProvisionMode::Check
    } else if args.dry_run {
        ProvisionMode::DryRun
    } else {
        ProvisionMode::Install
    };
    let reports = provision_envs(project_dir, &envs, mode, args.jobs.unwrap_or(1)).await?;
    if args.json {
        output::result(render_json(&reports));
    } else {
        output::result(render_text(&reports));
    }
    exit_status(&reports, mode)
}

/// `--all-envs`, the named envs, or the project's default env.
fn select_envs(project_dir: &Path, args: &InstallArgs) -> Result<Vec<String>> {
    let config = fbuild_config::PlatformIOConfig::from_path(&project_dir.join("platformio.ini"))?;
    if args.all_envs {
        return Ok(config
            .get_environments()
            .iter()
            .map(|env| env.to_string())
            .collect());
    }
    if !args.environments.is_empty() {
        return Ok(args.environments.clone());
    }
    config
        .get_default_environment()
        .map(|env| vec![env.to_string()])
        .ok_or_else(|| FbuildError::ConfigError("platformio.ini defines no environments".into()))
}

/// Provision each env, up to `jobs` at a time, keeping the envs' order.
async fn provision_envs(
    project_dir: &Path,
    envs: &[String],
    mode: ProvisionMode,
    jobs: usize,
) -> Result<Vec<ProvisionReport>> {
    use futures::stream::{self, StreamExt};
    stream::iter(
        envs.iter()
            .map(|env| fbuild_build::provision_env(project_dir, env, mode)),
    )
    .buffered(jobs.max(1))
    .collect::<Vec<_>>()
    .await
    .into_iter()
    .collect()
}

fn count(reports: &[ProvisionReport], status: ProvisionStatus) -> usize {
    reports
        .iter()
        .flat_map(|report| &report.packages)
        .filter(|package| package.status == status)
        .count()
}

fn render_text(reports: &[ProvisionReport]) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for report in reports {
        let _ = writeln!(out, "[{}] {}", report.env, report.platform);
        for package in &report.packages {
            let _ = writeln!(out, "  {}", package_line(package));
            if let Some(error) = &package.error {
                let _ = writeln!(out, "      error: {error}");
            }
        }
    }
    let total: usize = reports.iter().map(|report| report.packages.len()).sum();
    let bytes: u64 = reports
        .iter()
        .flat_map(|report| &report.packages)
        .filter_map(|package| package.bytes)
        .sum();
    let _ = write!(
        out,
        "{total} package(s): {} present, {} fetched, {} would-fetch, {} failed; {} on disk; packages_hash {}",
        count(reports, ProvisionStatus::Present),
        count(reports, ProvisionStatus::Fetched),
        count(reports, ProvisionStatus::WouldFetch),
        count(reports, ProvisionStatus::Failed),
        human_bytes(bytes),
        packages_hash(reports.iter().flat_map(|report| &report.packages)),
    );
    out
}

/// `status kind name version bytes duration url sha256`.
fn package_line(package: &ProvisionedPackage) -> String {
    fn or_dash(value: &str) -> &str {
        if value.is_empty() { "-" } else { value }
    }
    format!(
        "{:<11} {:<9} {} {} {} {}ms {} {}",
        package.status.as_str(),
        package.kind.as_str(),
        package.name,
        or_dash(&package.version),
        package.bytes.map_or_else(|| "-".to_string(), human_bytes),
        package.duration_ms,
        or_dash(&package.url),
        package.sha256.as_deref().unwrap_or("-"),
    )
}

/// The manifest CI keys caches on: every row plus a `packages_hash` per env
/// and across all of them.
fn render_json(reports: &[ProvisionReport]) -> String {
    let environments: Vec<_> = reports
        .iter()
        .map(|report| {
            serde_json::json!({
                "env": report.env,
                "platform": report.platform,
                "packages_hash": packages_hash(&report.packages),
                "packages": report.packages,
            })
        })
        .collect();
    let manifest = serde_json::json!({
        "packages_hash": packages_hash(reports.iter().flat_map(|report| &report.packages)),
        "environments": environments,
    });
    serde_json::to_string_pretty(&manifest)
        .expect("fbuild-cli: install manifest is built from serializable rows")
}

/// 1 when a package failed; [`CHECK_NEEDS_FETCH_EXIT`] when `--check` found
/// something missing; otherwise success.
fn exit_status(reports: &[ProvisionReport], mode: ProvisionMode) -> Result<()> {
    let failed = count(reports, ProvisionStatus::Failed);
    if failed > 0 {
        return Err(FbuildError::CommandFailed {
            message: format!("{failed} package(s) failed to install"),
            exit_code: 1,
        });
    }
    let missing = count(reports, ProvisionStatus::WouldFetch);
    if mode == ProvisionMode::Check && missing > 0 {
        return Err(FbuildError::CommandFailed {
            message: format!("{missing} package(s) would need fetching"),
            exit_code: CHECK_NEEDS_FETCH_EXIT,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fbuild_build::provision::PackageKind;

    fn args(environments: &[&str], all_envs: bool) -> InstallArgs {
        InstallArgs {
            project_dir: ".".into(),
            environments: environments.iter().map(|env| env.to_string()).collect(),
            all_envs,
            check: false,
            dry_run: false,
            json: false,
            jobs: None,
        }
    }

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("platformio.ini"),
            "[platformio]\ndefault_envs = uno\n\n[env:uno]\nplatform = atmelavr\nboard = uno\n\n[env:teensy41]\nplatform = teensy\nboard = teensy41\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn select_envs_prefers_all_then_named_then_default() {
        let dir = project();
        let mut all = select_envs(dir.path(), &args(&[], true)).unwrap();
        all.sort();
        assert_eq!(all, vec!["teensy41", "uno"]);
        assert_eq!(
            select_envs(dir.path(), &args(&["teensy41"], false)).unwrap(),
            vec!["teensy41"]
        );
        assert_eq!(
            select_envs(dir.path(), &args(&[], false)).unwrap(),
            vec!["uno"]
        );
    }

    fn report(statuses: &[ProvisionStatus]) -> ProvisionReport {
        ProvisionReport {
            env: "uno".into(),
            platform: "AtmelAvr".into(),
            packages: statuses
                .iter()
                .enumerate()
                .map(|(i, status)| ProvisionedPackage {
                    version: "1.0".into(),
                    url: format!("https://example.com/{i}"),
                    bytes: Some(2048),
                    error: (*status == ProvisionStatus::Failed).then(|| "404".to_string()),
                    ..ProvisionedPackage::new(PackageKind::Toolchain, format!("pkg-{i}"), *status)
                })
                .collect(),
        }
    }

    #[test]
    fn text_lists_each_package_and_a_summary() {
        let text = render_text(&[report(&[ProvisionStatus::Present, ProvisionStatus::Failed])]);
        assert!(text.contains("[uno] AtmelAvr"));
        assert!(text.contains("present     toolchain pkg-0 1.0 2.0 KB"));
        assert!(text.contains("error: 404"));
        assert!(text.contains("2 package(s): 1 present, 0 fetched, 0 would-fetch, 1 failed"));
    }

    #[test]
    fn json_manifest_carries_rows_and_hashes() {
        let reports = [report(&[ProvisionStatus::WouldFetch])];
        let manifest: serde_json::Value = serde_json::from_str(&render_json(&reports)).unwrap();
        let env = &manifest["environments"][0];
        assert_eq!(env["env"], "uno");
        assert_eq!(env["packages"][0]["status"], "would-fetch");
        assert_eq!(env["packages"][0]["kind"], "toolchain");
        assert_eq!(
            manifest["packages_hash"],
            packages_hash(&reports[0].packages)
        );
    }

    #[test]
    fn check_exits_two_only_when_something_would_be_fetched() {
        let missing = [report(&[ProvisionStatus::WouldFetch])];
        match exit_status(&missing, ProvisionMode::Check) {
            Err(FbuildError::CommandFailed { exit_code, .. }) => {
                assert_eq!(exit_code, CHECK_NEEDS_FETCH_EXIT)
            }
            other => panic!("expected exit 2, got {other:?}"),
        }
        assert!(exit_status(&missing, ProvisionMode::DryRun).is_ok());
        assert!(exit_status(&[report(&[ProvisionStatus::Present])], ProvisionMode::Check).is_ok());
    }

    #[test]
    fn a_failed_package_exits_one() {
        match exit_status(
            &[report(&[ProvisionStatus::Failed])],
            ProvisionMode::Install,
        ) {
            Err(FbuildError::CommandFailed { exit_code, .. }) => assert_eq!(exit_code, 1),
            other => panic!("expected exit 1, got {other:?}"),
        }
    }
}
